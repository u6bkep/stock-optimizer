#!/usr/bin/env bash
set -euo pipefail

DEPLOY_BRANCH="deploy"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Ensure we're on a source branch, not deploy
CURRENT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
if [ "$CURRENT_BRANCH" = "$DEPLOY_BRANCH" ]; then
  echo "Error: run this from a source branch, not $DEPLOY_BRANCH"
  exit 1
fi

SOURCE_REF="$(git rev-parse --short HEAD)"

echo "==> Building WASM..."
wasm-pack build --target web --out-dir pkg

echo "==> Preparing deploy worktree..."
WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

git worktree add "$WORK_DIR" "$DEPLOY_BRANCH"

# Clear old content (keep .git worktree link)
find "$WORK_DIR" -mindepth 1 -maxdepth 1 ! -name '.git' -exec rm -rf {} +

# Copy static site files
cp "$SCRIPT_DIR/index.html" "$WORK_DIR/"
cp -r "$SCRIPT_DIR/pkg" "$WORK_DIR/pkg"

# Remove files we don't need served
rm -f "$WORK_DIR/pkg/.gitignore" "$WORK_DIR/pkg/package.json" \
      "$WORK_DIR/pkg/"*.d.ts

cd "$WORK_DIR"
git add -A

if git diff --cached --quiet; then
  echo "==> No changes to deploy."
else
  git commit -m "Deploy from $CURRENT_BRANCH @ $SOURCE_REF"
  echo "==> Committed to $DEPLOY_BRANCH."
fi

cd "$SCRIPT_DIR"
git worktree remove "$WORK_DIR"

echo "==> Done. Push with: git push origin $DEPLOY_BRANCH"
