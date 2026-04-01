use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

// Platform-agnostic time: uses performance.now() on WASM, std::time on native.
#[derive(Clone, Copy)]
struct Deadline(f64); // milliseconds since some epoch

impl Deadline {
    fn after_secs(secs: f64) -> Self {
        Deadline(now_ms() + secs * 1000.0)
    }

    fn is_past(&self) -> bool {
        now_ms() >= self.0
    }
}

#[cfg(target_arch = "wasm32")]
fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        * 1000.0
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    /// Length of each stock bar
    pub stock_length: f64,
    /// Width of material lost per cut (saw kerf)
    pub kerf: f64,
    /// Parts to cut: each entry is (length, quantity)
    pub parts: Vec<PartSpec>,
    /// Time budget in seconds for the main solve (default: 5)
    #[serde(default = "default_solve_seconds")]
    pub solve_seconds: f64,
    /// Max extra bars to explore for overproduction suggestions (default: 3)
    #[serde(default = "default_max_extra_bars")]
    pub max_extra_bars: usize,
    /// Time budget in seconds for the suggestion search (default: 5)
    #[serde(default = "default_suggest_seconds")]
    pub suggest_seconds: f64,
}

fn default_solve_seconds() -> f64 {
    5.0
}

fn default_max_extra_bars() -> usize {
    3
}

fn default_suggest_seconds() -> f64 {
    5.0
}

impl Config {
    pub fn new(stock_length: f64, kerf: f64, parts: Vec<PartSpec>) -> Self {
        Self {
            stock_length,
            kerf,
            parts,
            solve_seconds: default_solve_seconds(),
            max_extra_bars: default_max_extra_bars(),
            suggest_seconds: default_suggest_seconds(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PartSpec {
    pub length: f64,
    pub qty: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Solution {
    pub bars: Vec<Bar>,
    pub stats: Stats,
    pub suggestions: Vec<Suggestion>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    /// Adjusted quantities for each part type
    pub quantities: Vec<u32>,
    /// Delta vs. original demand per part type (positive = extra, negative = deferred)
    pub extra: Vec<i32>,
    /// Number of bars needed
    pub total_bars: usize,
    /// Extra bars vs. baseline (0 = fits in same count, negative = saves bars)
    pub extra_bars: i32,
    pub efficiency_pct: f64,
    pub total_waste: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Bar {
    /// Which cuts go on this bar, sorted longest first
    pub cuts: Vec<Cut>,
    /// Material used (parts + kerf between them)
    pub used: f64,
    /// Leftover material
    pub waste: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Cut {
    pub part_index: usize,
    pub length: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub total_bars: usize,
    pub efficiency_pct: f64,
    pub total_waste: f64,
    pub total_parts_cut: u32,
    pub patterns_generated: usize,
    pub solve_method: String,
}

/// A pattern is a vector of counts — how many of each part type fit on one bar.
type Pattern = Vec<u32>;

pub fn optimize(config: &Config) -> Result<Solution, String> {
    validate(config)?;

    let lengths: Vec<f64> = config.parts.iter().map(|p| p.length).collect();
    let demand: Vec<u32> = config.parts.iter().map(|p| p.qty).collect();

    // Patterns depend only on lengths, stock_length, kerf — reusable across demand variants
    let patterns = gen_patterns(&lengths, config.stock_length, config.kerf);
    if patterns.is_empty() {
        return Err("No valid cutting patterns found".into());
    }

    let deadline = Deadline::after_secs(config.solve_seconds);
    let (assignment, exact) =
        bnb_solve(&patterns, &demand, config.stock_length, &lengths, config.kerf, deadline);

    let bars = assignment_to_bars(&assignment, &lengths, config.stock_length, config.kerf);
    let stats = compute_stats(&bars, config.stock_length, patterns.len(), exact);

    let suggestions = find_suggestions(
        &patterns,
        &demand,
        config.stock_length,
        &lengths,
        config.kerf,
        stats.total_bars,
        stats.efficiency_pct,
        config.max_extra_bars,
        config.suggest_seconds,
    );

    Ok(Solution {
        bars,
        stats,
        suggestions,
    })
}

fn assignment_to_bars(
    assignment: &[Pattern],
    lengths: &[f64],
    stock_length: f64,
    kerf: f64,
) -> Vec<Bar> {
    assignment
        .iter()
        .map(|pattern| build_bar(pattern, lengths, stock_length, kerf))
        .collect()
}

fn compute_stats(bars: &[Bar], stock_length: f64, patterns_generated: usize, exact: bool) -> Stats {
    let total_parts_material: f64 = bars
        .iter()
        .map(|b| b.cuts.iter().map(|c| c.length).sum::<f64>())
        .sum();
    let total_stock = bars.len() as f64 * stock_length;

    let method = if exact {
        "branch-and-bound (optimal)"
    } else {
        "branch-and-bound (best found in 5s)"
    };

    Stats {
        total_bars: bars.len(),
        efficiency_pct: if total_stock > 0.0 {
            total_parts_material / total_stock * 100.0
        } else {
            0.0
        },
        total_waste: bars.iter().map(|b| b.waste).sum(),
        total_parts_cut: bars.iter().map(|b| b.cuts.len() as u32).sum(),
        patterns_generated,
        solve_method: method.into(),
    }
}

/// Find suggestions by adjusting part counts both up and down.
///
/// **Overproduction** (+ direction): for each bar count from baseline to
/// baseline + max_extra_bars, find the maximum extra parts that fit.
///
/// **Underproduction** (- direction): try reducing each part type to see
/// if deferring a few parts to another run saves bars / improves efficiency.
fn find_suggestions(
    patterns: &[Pattern],
    demand: &[u32],
    stock_length: f64,
    lengths: &[f64],
    kerf: f64,
    baseline_bars: usize,
    baseline_efficiency: f64,
    max_extra_bars: usize,
    suggest_seconds: f64,
) -> Vec<Suggestion> {
    let n = demand.len();
    let deadline = Deadline::after_secs(suggest_seconds);
    let mut suggestions: Vec<Suggestion> = Vec::new();

    // --- Overproduction: search + direction ---
    for extra_bar in 0..=max_extra_bars {
        let target_bars = baseline_bars + extra_bar;
        if deadline.is_past() {
            break;
        }
        if let Some(s) = best_overproduction_at(
            patterns, demand, stock_length, lengths, kerf,
            target_bars, baseline_bars, deadline,
        ) {
            suggestions.push(s);
        }
    }

    // --- Underproduction: search - direction ---
    // For each part type, try removing 1..N and see if we get fewer bars
    // or better efficiency with fewer bars.
    // Also try combined reductions.
    let mut best_under: Option<Suggestion> = None;

    for i in 0..n {
        if deadline.is_past() {
            break;
        }
        for delta in 1..=demand[i] {
            if deadline.is_past() {
                break;
            }
            let mut trial = demand.to_vec();
            trial[i] -= delta;
            // Skip if zero total parts
            if trial.iter().all(|&q| q == 0) {
                break;
            }
            let (assignment, _) =
                bnb_solve(patterns, &trial, stock_length, lengths, kerf, deadline);
            let bars = assignment_to_bars(&assignment, lengths, stock_length, kerf);
            let s = make_suggestion(&trial, demand, &bars, stock_length, baseline_bars);

            // Only interesting if it saves bars or improves efficiency
            if s.total_bars < baseline_bars || s.efficiency_pct > baseline_efficiency + 0.5 {
                let dominated = best_under.as_ref().is_some_and(|prev| {
                    prev.total_bars <= s.total_bars && prev.efficiency_pct >= s.efficiency_pct
                });
                if !dominated {
                    best_under = Some(s);
                }
            }

            // If removing more doesn't keep reducing bars, stop
            if assignment.len() >= baseline_bars && delta > 1 {
                break;
            }
        }
    }

    // Try combined underproduction: reduce all types proportionally
    // by the amount that the single-best reduction found
    if !deadline.is_past() && n >= 2 {
        // Try reducing each type by 1 simultaneously
        let mut trial = demand.to_vec();
        let mut any_reduced = false;
        for i in 0..n {
            if trial[i] > 1 {
                trial[i] -= 1;
                any_reduced = true;
            }
        }
        if any_reduced && trial.iter().any(|&q| q > 0) {
            let (assignment, _) =
                bnb_solve(patterns, &trial, stock_length, lengths, kerf, deadline);
            let bars = assignment_to_bars(&assignment, lengths, stock_length, kerf);
            let s = make_suggestion(&trial, demand, &bars, stock_length, baseline_bars);
            if s.total_bars < baseline_bars || s.efficiency_pct > baseline_efficiency + 0.5 {
                let dominated = best_under.as_ref().is_some_and(|prev| {
                    prev.total_bars <= s.total_bars && prev.efficiency_pct >= s.efficiency_pct
                });
                if !dominated {
                    best_under = Some(s);
                }
            }
        }
    }

    if let Some(s) = best_under {
        suggestions.push(s);
    }

    // Remove entries with no change
    suggestions.retain(|s| s.extra.iter().any(|&e| e != 0));

    // Sort by efficiency descending
    suggestions.sort_by(|a, b| {
        b.efficiency_pct
            .partial_cmp(&a.efficiency_pct)
            .unwrap()
    });

    // Deduplicate by quantities
    suggestions.dedup_by(|a, b| a.quantities == b.quantities);

    suggestions
}

/// Find the best combined overproduction that fits in `target_bars` bars.
fn best_overproduction_at(
    patterns: &[Pattern],
    demand: &[u32],
    stock_length: f64,
    lengths: &[f64],
    kerf: f64,
    target_bars: usize,
    baseline_bars: usize,
    deadline: Deadline,
) -> Option<Suggestion> {
    let n = demand.len();

    // Binary search per part type: max extras that fit in target_bars
    let mut per_type_max = vec![0u32; n];
    for i in 0..n {
        if deadline.is_past() {
            break;
        }
        let total_capacity = target_bars as f64 * stock_length;
        let current_material: f64 =
            demand.iter().zip(lengths).map(|(&q, &l)| q as f64 * l).sum();
        let upper = ((total_capacity - current_material) / lengths[i]).floor() as u32;
        if upper == 0 {
            continue;
        }

        let (mut lo, mut hi) = (0u32, upper);
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            let mut trial = demand.to_vec();
            trial[i] += mid;
            let (assignment, _) =
                bnb_solve(patterns, &trial, stock_length, lengths, kerf, deadline);
            if assignment.len() <= target_bars {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        per_type_max[i] = lo;
    }

    if per_type_max.iter().all(|&m| m == 0) {
        return None;
    }

    // Try all per-type maxes combined
    let mut best_trial = demand.to_vec();
    for i in 0..n {
        best_trial[i] += per_type_max[i];
    }

    let (assignment, _) =
        bnb_solve(patterns, &best_trial, stock_length, lengths, kerf, deadline);
    if assignment.len() <= target_bars {
        let bars = assignment_to_bars(&assignment, lengths, stock_length, kerf);
        let s = make_suggestion(&best_trial, demand, &bars, stock_length, baseline_bars);
        if s.extra.iter().any(|&e| e > 0) {
            return Some(s);
        }
    }

    // All-at-once doesn't fit. Greedy: add one part type at a time.
    let mut order: Vec<usize> = (0..n).filter(|&i| per_type_max[i] > 0).collect();
    order.sort_by(|&a, &b| {
        (per_type_max[b] as f64 * lengths[b])
            .partial_cmp(&(per_type_max[a] as f64 * lengths[a]))
            .unwrap()
    });

    let mut combined = demand.to_vec();
    for &i in &order {
        if deadline.is_past() {
            break;
        }
        let (mut lo, mut hi) = (0u32, per_type_max[i]);
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            let mut trial = combined.clone();
            trial[i] += mid;
            let (assignment, _) =
                bnb_solve(patterns, &trial, stock_length, lengths, kerf, deadline);
            if assignment.len() <= target_bars {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        combined[i] += lo;
    }

    let has_extra = combined.iter().zip(demand).any(|(&c, &d)| c > d);
    if !has_extra {
        return None;
    }

    let (assignment, _) =
        bnb_solve(patterns, &combined, stock_length, lengths, kerf, deadline);
    let bars = assignment_to_bars(&assignment, lengths, stock_length, kerf);
    Some(make_suggestion(&combined, demand, &bars, stock_length, baseline_bars))
}

fn make_suggestion(
    trial: &[u32],
    demand: &[u32],
    bars: &[Bar],
    stock_length: f64,
    baseline_bars: usize,
) -> Suggestion {
    let bar_count = bars.len();
    let total_waste: f64 = bars.iter().map(|b| b.waste).sum();
    let total_parts_material: f64 = bars
        .iter()
        .map(|b| b.cuts.iter().map(|c| c.length).sum::<f64>())
        .sum();
    let efficiency = total_parts_material / (bar_count as f64 * stock_length) * 100.0;
    let extra: Vec<i32> = trial
        .iter()
        .zip(demand)
        .map(|(&t, &d)| t as i32 - d as i32)
        .collect();

    Suggestion {
        quantities: trial.to_vec(),
        extra,
        total_bars: bar_count,
        extra_bars: bar_count as i32 - baseline_bars as i32,
        efficiency_pct: efficiency,
        total_waste,
    }
}

fn validate(config: &Config) -> Result<(), String> {
    if config.stock_length <= 0.0 {
        return Err("Stock length must be positive".into());
    }
    if config.kerf < 0.0 {
        return Err("Kerf must be non-negative".into());
    }
    if config.parts.is_empty() {
        return Err("At least one part is required".into());
    }
    for (i, p) in config.parts.iter().enumerate() {
        if p.length <= 0.0 {
            return Err(format!("Part {} has non-positive length", i + 1));
        }
        if p.qty == 0 {
            return Err(format!("Part {} has zero quantity", i + 1));
        }
        if p.length > config.stock_length {
            return Err(format!(
                "Part {} ({}\") exceeds stock length ({}\")",
                i + 1,
                p.length,
                config.stock_length
            ));
        }
    }
    Ok(())
}

/// Generate all valid cutting patterns.
///
/// A pattern is an array of counts (one per part type) that fits within one stock bar.
/// Each cut after the first on a bar costs an additional `kerf` of material.
fn gen_patterns(lengths: &[f64], stock_length: f64, kerf: f64) -> Vec<Pattern> {
    let mut patterns: Vec<Pattern> = Vec::new();
    let mut counts = vec![0u32; lengths.len()];

    recurse_patterns(
        0,
        stock_length,
        0,
        &mut counts,
        lengths,
        kerf,
        &mut patterns,
    );
    patterns
}

fn recurse_patterns(
    idx: usize,
    remaining: f64,
    total_pieces: u32,
    counts: &mut Vec<u32>,
    lengths: &[f64],
    kerf: f64,
    patterns: &mut Vec<Pattern>,
) {
    if idx == lengths.len() {
        if total_pieces > 0 {
            patterns.push(counts.clone());
        }
        return;
    }

    // How many of part[idx] can we fit given the remaining space?
    // The first piece ever placed on the bar has no kerf cost.
    // Every subsequent piece costs kerf + length.
    let max_k = {
        if total_pieces == 0 {
            // Placing k pieces of this type first on an empty bar:
            // cost = length * k + kerf * (k - 1)  for k >= 1
            //      = k * (length + kerf) - kerf
            // remaining >= k * (length + kerf) - kerf
            // k <= (remaining + kerf) / (length + kerf)
            if lengths[idx] + kerf > 0.0 {
                ((remaining + kerf) / (lengths[idx] + kerf)).floor() as u32
            } else {
                0
            }
        } else {
            // Bar already has pieces; each new piece costs kerf + length
            if kerf + lengths[idx] > 0.0 {
                (remaining / (kerf + lengths[idx])).floor() as u32
            } else {
                0
            }
        }
    };

    for k in 0..=max_k {
        counts[idx] = k;
        let space_used = if k == 0 {
            0.0
        } else if total_pieces == 0 {
            // First pieces on the bar: k * length + (k-1) * kerf
            lengths[idx] * k as f64 + kerf * (k - 1) as f64
        } else {
            // Bar already has pieces: each new one costs kerf + length
            (kerf + lengths[idx]) * k as f64
        };
        recurse_patterns(
            idx + 1,
            remaining - space_used,
            total_pieces + k,
            counts,
            lengths,
            kerf,
            patterns,
        );
    }
    counts[idx] = 0;
}

/// Compute how much material a pattern uses on a bar.
fn pattern_material(pattern: &[u32], lengths: &[f64], kerf: f64) -> f64 {
    let total_pieces: u32 = pattern.iter().sum();
    if total_pieces == 0 {
        return 0.0;
    }
    let part_material: f64 = pattern
        .iter()
        .zip(lengths)
        .map(|(&c, &l)| c as f64 * l)
        .sum();
    let kerf_material = kerf * (total_pieces - 1) as f64;
    part_material + kerf_material
}

/// Best Fit Decreasing heuristic — produces a quick feasible solution.
fn bfd(lengths: &[f64], demand: &[u32], stock_length: f64, kerf: f64) -> Vec<Pattern> {
    let n = lengths.len();

    // Expand demand into individual part indices, sorted by length descending
    let mut all_parts: Vec<usize> = Vec::new();
    for (i, &qty) in demand.iter().enumerate() {
        for _ in 0..qty {
            all_parts.push(i);
        }
    }
    all_parts.sort_by(|&a, &b| lengths[b].partial_cmp(&lengths[a]).unwrap());

    struct OpenBar {
        counts: Vec<u32>,
        pieces: u32,
        remaining: f64,
    }

    let mut bars: Vec<OpenBar> = Vec::new();

    for &pi in &all_parts {
        let cost_existing = kerf + lengths[pi];
        let cost_new = lengths[pi];

        let mut best_idx: Option<usize> = None;
        let mut best_rem = f64::INFINITY;

        for (bi, bar) in bars.iter().enumerate() {
            let cost = if bar.pieces == 0 { cost_new } else { cost_existing };
            let rem_after = bar.remaining - cost;
            if rem_after >= -1e-9 && rem_after < best_rem {
                best_rem = rem_after;
                best_idx = Some(bi);
            }
        }

        if let Some(bi) = best_idx {
            let bar = &mut bars[bi];
            let cost = if bar.pieces == 0 { cost_new } else { cost_existing };
            bar.remaining -= cost;
            bar.counts[pi] += 1;
            bar.pieces += 1;
        } else {
            let mut counts = vec![0u32; n];
            counts[pi] = 1;
            bars.push(OpenBar {
                counts,
                pieces: 1,
                remaining: stock_length - cost_new,
            });
        }
    }

    bars.into_iter().map(|b| b.counts).collect()
}

/// Branch-and-bound solver that decides *how many times* to use each pattern.
///
/// Instead of choosing a pattern for each bar (which has permutation symmetry and
/// explodes combinatorially), we iterate over patterns and decide how many copies
/// of each to use. This eliminates symmetry and makes the search tractable.
///
/// Returns (assignment, exact) where exact=true if the search completed fully.
fn bnb_solve(
    patterns: &[Pattern],
    demand: &[u32],
    stock_length: f64,
    lengths: &[f64],
    kerf: f64,
    deadline: Deadline,
) -> (Vec<Pattern>, bool) {
    let n = demand.len();

    // Start with BFD as upper bound
    let bfd_result = bfd(lengths, demand, stock_length, kerf);
    let mut best_count = bfd_result.len();
    let mut best_multiplicities: Vec<u32> = Vec::new(); // empty = use bfd_result directly
    let mut used_bfd = true;

    // Sort patterns by material used (descending) — try dense patterns first.
    // Also filter out dominated patterns (where another pattern is >= in every dimension).
    let mut scored: Vec<(usize, f64)> = patterns
        .iter()
        .enumerate()
        .map(|(i, p)| (i, pattern_material(p, lengths, kerf)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let sorted_indices: Vec<usize> = scored.iter().map(|&(i, _)| i).collect();

    // Filter dominated patterns: pattern A dominates B if A[i] >= B[i] for all i
    // and A != B. Keep only non-dominated patterns.
    let mut keep = vec![true; sorted_indices.len()];
    for (si, &ai) in sorted_indices.iter().enumerate() {
        if !keep[si] {
            continue;
        }
        for (sj, &bj) in sorted_indices.iter().enumerate() {
            if si == sj || !keep[sj] {
                continue;
            }
            // Does pattern ai dominate pattern bj?
            if patterns[ai]
                .iter()
                .zip(patterns[bj].iter())
                .all(|(&a, &b)| a >= b)
                && patterns[ai] != patterns[bj]
            {
                keep[sj] = false;
            }
        }
    }
    let filtered: Vec<usize> = sorted_indices
        .iter()
        .enumerate()
        .filter(|&(si, _)| keep[si])
        .map(|(_, &i)| i)
        .collect();

    // Precompute: for each part type, the max count achievable by any remaining
    // pattern from index pi onward (used for the lower bound).
    // max_from[pi][j] = max of patterns[filtered[pi..]][j]
    let np = filtered.len();
    let mut max_from: Vec<Vec<u32>> = vec![vec![0; n]; np + 1];
    for pi in (0..np).rev() {
        let pat = &patterns[filtered[pi]];
        for j in 0..n {
            max_from[pi][j] = max_from[pi + 1][j].max(pat[j]);
        }
    }

    let mut multiplicities: Vec<u32> = vec![0; np];
    let mut remaining: Vec<i32> = demand.iter().map(|&d| d as i32).collect();
    let mut total_bars: usize = 0;
    let mut timed_out = false;
    let mut nodes: u64 = 0;

    bnb_search(
        0,
        &filtered,
        patterns,
        &mut remaining,
        &mut total_bars,
        &mut multiplicities,
        &mut best_count,
        &mut best_multiplicities,
        &mut used_bfd,
        &max_from,
        deadline,
        &mut timed_out,
        &mut nodes,
    );

    // Expand the best result into a flat list of patterns
    if used_bfd {
        return (bfd_result, !timed_out);
    }

    let mut assignment: Vec<Pattern> = Vec::new();
    for (si, &count) in best_multiplicities.iter().enumerate() {
        for _ in 0..count {
            assignment.push(patterns[filtered[si]].clone());
        }
    }
    (assignment, !timed_out)
}

/// Lower bound: for each part type with remaining demand, compute
/// ceil(remaining / max_achievable_per_bar) using only patterns from
/// index `from_pat` onward. Take the max across all part types.
fn lower_bound_from(remaining: &[i32], max_from: &[u32]) -> usize {
    let mut lb: usize = 0;
    for (i, &r) in remaining.iter().enumerate() {
        if r <= 0 {
            continue;
        }
        let m = max_from[i];
        if m == 0 {
            return usize::MAX;
        }
        let needed = (r as u32 + m - 1) / m;
        lb = lb.max(needed as usize);
    }
    lb
}

fn bnb_search(
    pat_idx: usize,
    filtered: &[usize],
    patterns: &[Pattern],
    remaining: &mut Vec<i32>,
    total_bars: &mut usize,
    multiplicities: &mut Vec<u32>,
    best_count: &mut usize,
    best_multiplicities: &mut Vec<u32>,
    used_bfd: &mut bool,
    max_from: &[Vec<u32>],
    deadline: Deadline,
    timed_out: &mut bool,
    nodes: &mut u64,
) {
    if *timed_out {
        return;
    }

    *nodes += 1;
    if *nodes % 10_000 == 0 && deadline.is_past() {
        *timed_out = true;
        return;
    }

    // All demand satisfied?
    if remaining.iter().all(|&r| r <= 0) {
        if *total_bars < *best_count {
            *best_count = *total_bars;
            *best_multiplicities = multiplicities.clone();
            *used_bfd = false;
        }
        return;
    }

    // No more patterns to try?
    if pat_idx >= filtered.len() {
        return;
    }

    // Prune: even with the best remaining patterns, can we beat best_count?
    let lb = lower_bound_from(remaining, &max_from[pat_idx]);
    if lb == usize::MAX || *total_bars + lb >= *best_count {
        return;
    }

    let pat = &patterns[filtered[pat_idx]];

    // Upper bound on how many times we could use this pattern:
    // limited by demand (don't overshoot by more than necessary) and bar budget.
    let max_uses = {
        let mut max_by_demand = *best_count - *total_bars - 1; // must leave room to improve
        for (j, &r) in remaining.iter().enumerate() {
            if pat[j] > 0 && r > 0 {
                // At most ceil(r / pat[j]) copies needed for this part type
                let needed = (r as u32 + pat[j] - 1) / pat[j];
                max_by_demand = max_by_demand.min(needed as usize);
            }
        }
        max_by_demand
    };

    // Try using this pattern k times (from max down to 0 — try high usage first)
    for k in (0..=max_uses).rev() {
        multiplicities[pat_idx] = k as u32;
        *total_bars += k;
        for (j, &c) in pat.iter().enumerate() {
            remaining[j] -= (k as u32 * c) as i32;
        }

        bnb_search(
            pat_idx + 1,
            filtered,
            patterns,
            remaining,
            total_bars,
            multiplicities,
            best_count,
            best_multiplicities,
            used_bfd,
            max_from,
            deadline,
            timed_out,
            nodes,
        );

        for (j, &c) in pat.iter().enumerate() {
            remaining[j] += (k as u32 * c) as i32;
        }
        *total_bars -= k;
    }
    multiplicities[pat_idx] = 0;
}

fn build_bar(pattern: &Pattern, lengths: &[f64], stock_length: f64, kerf: f64) -> Bar {
    let mut cuts: Vec<Cut> = Vec::new();
    for (i, &count) in pattern.iter().enumerate() {
        for _ in 0..count {
            cuts.push(Cut {
                part_index: i,
                length: lengths[i],
            });
        }
    }
    cuts.sort_by(|a, b| b.length.partial_cmp(&a.length).unwrap());

    let used = pattern_material(pattern, lengths, kerf);
    let waste = stock_length - used;

    Bar { cuts, used, waste }
}

/// WASM entry point: takes JSON config string, returns JSON solution string.
#[wasm_bindgen]
pub fn optimize_json(config_json: &str) -> Result<String, String> {
    let config: Config =
        serde_json::from_str(config_json).map_err(|e| format!("Invalid config: {e}"))?;
    let solution = optimize(&config)?;
    serde_json::to_string(&solution).map_err(|e| format!("Serialization error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a config with suggestions disabled (fast tests)
    fn cfg(stock_length: f64, kerf: f64, parts: Vec<PartSpec>) -> Config {
        Config {
            stock_length,
            kerf,
            parts,
            solve_seconds: 5.0,
            max_extra_bars: 0,
            suggest_seconds: 0.0,
        }
    }

    #[test]
    fn test_simple_fit() {
        let sol = optimize(&cfg(72.0, 0.125, vec![
            PartSpec { length: 35.0, qty: 2 },
        ])).unwrap();
        assert_eq!(sol.bars.len(), 1);
        assert_eq!(sol.bars[0].cuts.len(), 2);
    }

    #[test]
    fn test_doesnt_fit_one_bar() {
        let sol = optimize(&cfg(72.0, 0.125, vec![
            PartSpec { length: 37.0, qty: 2 },
        ])).unwrap();
        assert_eq!(sol.bars.len(), 2);
    }

    #[test]
    fn test_default_example() {
        let config = cfg(72.0, 0.125, vec![
            PartSpec { length: 12.0, qty: 4 },
            PartSpec { length: 16.0, qty: 3 },
            PartSpec { length: 20.0, qty: 2 },
            PartSpec { length: 24.0, qty: 2 },
        ]);
        let sol = optimize(&config).unwrap();
        assert!(sol.bars.len() >= 3);
        assert!(sol.bars.len() <= 4);

        let mut cut_counts = vec![0u32; config.parts.len()];
        for bar in &sol.bars {
            for cut in &bar.cuts {
                cut_counts[cut.part_index] += 1;
            }
        }
        for (i, p) in config.parts.iter().enumerate() {
            assert!(cut_counts[i] >= p.qty, "Part {} not fully satisfied", i);
        }
    }

    #[test]
    fn test_kerf_accounting() {
        let sol = optimize(&cfg(10.0, 1.0, vec![
            PartSpec { length: 3.0, qty: 3 },
        ])).unwrap();
        assert_eq!(sol.bars.len(), 2);
    }

    #[test]
    fn test_zero_kerf() {
        let sol = optimize(&cfg(10.0, 0.0, vec![
            PartSpec { length: 2.5, qty: 8 },
        ])).unwrap();
        assert_eq!(sol.bars.len(), 2);
    }

    #[test]
    fn test_part_exceeds_stock() {
        assert!(optimize(&cfg(10.0, 0.0, vec![
            PartSpec { length: 11.0, qty: 1 },
        ])).is_err());
    }

    #[test]
    fn test_json_roundtrip() {
        let json = r#"{
            "stock_length": 72,
            "kerf": 0.125,
            "parts": [
                {"length": 12, "qty": 4},
                {"length": 16, "qty": 3}
            ]
        }"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.solve_seconds, 5.0); // default applied
        assert_eq!(config.max_extra_bars, 3); // default applied
        assert_eq!(config.suggest_seconds, 5.0); // default applied
    }

    #[test]
    fn test_waste_is_non_negative() {
        let sol = optimize(&cfg(72.0, 0.125, vec![
            PartSpec { length: 12.0, qty: 4 },
            PartSpec { length: 16.0, qty: 3 },
            PartSpec { length: 20.0, qty: 2 },
            PartSpec { length: 24.0, qty: 2 },
        ])).unwrap();
        for bar in &sol.bars {
            assert!(bar.waste >= -1e-9, "Bar has negative waste: {}", bar.waste);
        }
    }
}
