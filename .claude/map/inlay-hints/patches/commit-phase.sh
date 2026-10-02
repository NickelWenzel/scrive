#!/usr/bin/env bash
# Usage: commit-phase.sh <phase-number> <base-commit> [--all-features]
# Turns the phase agent's cumulative patches (phase<N>-<k>.patch/.msg) into commits on top of
# <base>. It verifies every commit in a throwaway worktree, then advances the current branch.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
N=$1 BASE=$2 FEAT=${3:-}
P=.claude/map/inlay-hints/patches
T=/tmp/claude-1000
mkdir -p "$T"
branch=$(git symbolic-ref --short HEAD)
[ "$(git rev-parse HEAD)" = "$(git rev-parse "$BASE")" ] || { echo "HEAD is not $BASE"; exit 1; }
last=$(ls $P/phase$N-*.patch | sed -E 's/.*-([0-9]+)\.patch/\1/' | sort -n | tail -1)

# The working tree must equal the final patch. New files need intent-to-add so git diff sees them.
git add -N . >/dev/null 2>&1 || true
git diff "$BASE" -- . ':(exclude).claude' | cmp -s - "$P/phase$N-$last.patch" \
  || { git reset -q; echo "working tree != phase$N-$last.patch"; exit 1; }
git reset -q

parent=$(git rev-parse "$BASE")
for k in $(seq 1 "$last"); do
  idx="$T/idx-$N-$k"; rm -f "$idx"
  GIT_INDEX_FILE="$idx" git read-tree "$BASE"
  GIT_INDEX_FILE="$idx" git apply --cached "$P/phase$N-$k.patch"
  tree=$(GIT_INDEX_FILE="$idx" git write-tree)
  parent=$(git commit-tree "$tree" -p "$parent" -F "$P/phase$N-$k.msg")
done
tip=$parent

WT="$T/wt-$N"; git worktree remove --force "$WT" 2>/dev/null || true
git worktree add -q --detach "$WT" "$tip"
export CARGO_TARGET_DIR="$PWD/target"
fail=0
for c in $(git rev-list --reverse "$BASE".."$tip"); do
  git -C "$WT" checkout -q "$c"
  subj=$(git log --format=%s -1 "$c")
  (cd "$WT" && cargo clippy --workspace --all-targets -q -- -D warnings) >"$T/clippy.log" 2>&1 || { echo "CLIPPY FAIL: $subj"; tail -20 "$T/clippy.log"; fail=1; break; }
  if [ "$FEAT" = "--all-features" ]; then
    (cd "$WT" && cargo clippy --workspace --all-targets --all-features -q -- -D warnings) >"$T/clippy.log" 2>&1 || { echo "CLIPPY(all) FAIL: $subj"; tail -20 "$T/clippy.log"; fail=1; break; }
  fi
  res=$(cd "$WT" && cargo test --workspace $FEAT 2>&1 | grep -E "^test result|error\[|could not compile" || true)
  echo "$res" | grep -qE "could not compile|error\[| [1-9][0-9]* failed" && { echo "TEST FAIL: $subj"; echo "$res" | tail -5; fail=1; break; }
  echo "ok  $(echo "$res" | awk '/^test result/ {p+=$4} END {print p}') tests | $subj"
done
git worktree remove --force "$WT"
[ $fail = 0 ] || { echo "not advancing $branch"; exit 1; }
git update-ref "refs/heads/$branch" "$tip" "$BASE"
git reset -q
git log --oneline "$BASE".."$tip"
