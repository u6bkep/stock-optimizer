# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Bar stock cut optimizer — finds optimal ways to cut parts from fixed-length stock bars, minimizing waste. Dual-target Rust codebase: compiles to WASM for browser use and native CLI binary.

## Build Commands

```bash
# WASM build (web)
wasm-pack build --target web --out-dir pkg

# Native build
cargo build --release

# Run tests
cargo test

# Run native CLI
./target/release/stock-optimizer config.json
# or: ./target/release/stock-optimizer < config.json

# Deploy (builds WASM, copies to deploy branch via git worktree)
./deploy.sh
```

## Architecture

Single-crate Rust project with two entry points:

- **`src/lib.rs`** — All optimization logic and types. WASM entry point (`optimize_json`) wraps the core `optimize()` function with JSON I/O via `wasm-bindgen`.
- **`src/main.rs`** — Native CLI entry point. Reads JSON config from file/stdin, calls `optimize()`, prints JSON solution.
- **`index.html`** — Vanilla JS single-page frontend. Loads WASM, renders SVG bar visualizations, manages URL-based state persistence via query params.

### Optimization Pipeline

`optimize(config)` orchestrates:
1. **`gen_patterns()`** — Recursive backtracking enumerates all valid cutting patterns (combinations of part counts per bar, accounting for kerf). Filters dominated patterns.
2. **`bnb_solve()`** — Branch-and-bound over pattern *multiplicities* (how many times each pattern is used), not per-bar permutations. Initial upper bound from Best Fit Decreasing heuristic (`bfd()`). Deadline-aware — uses `web_sys::Performance` in WASM, `std::time` natively.
3. **`compute_naive()`** — Baseline: single-size cuts per bar for comparison.
4. **`find_suggestions()`** — Explores overproduction (extra parts that fit) and underproduction (fewer parts to save bars/waste).

### Key Types

- `Config` — input: stock_length, kerf, parts (Vec<PartSpec>), solve budget params
- `Pattern` = `Vec<u32>` — count of each part type on a single bar
- `Solution` — output: bars, stats, naive baseline, suggestion list

## Deployment

Source lives on `master`. Built artifacts (index.html + pkg/) go to the `deploy` branch via `deploy.sh`, which uses a git worktree. After running `deploy.sh`, manually `git push origin deploy`.
