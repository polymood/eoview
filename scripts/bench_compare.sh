#!/bin/sh
# Compare the open benchmark (crates/eo-cache/benches/open.rs) of the last release (the last v* tag) with
# the current tree. The release builds in a git worktree in target/bench-release. The two builds use the
# same target directory, thus the baselines of criterion are in target/criterion.
# EOVIEW_BENCH_FILES: more files (see open.rs). Use a memory limit, for example:
#   CARGO_BUILD_JOBS=4 systemd-run --user --scope -q -p MemoryMax=6G scripts/bench_compare.sh
set -e
cd "$(git rev-parse --show-toplevel)"
tag=$(git tag -l 'v*' --sort=-v:refname | head -n 1)
wt=target/bench-release
[ -d "$wt" ] || git worktree add -q --detach "$wt" "$tag"
git -C "$wt" checkout -q --detach "$tag"
export CARGO_TARGET_DIR="$PWD/target"
echo "== $tag"
(cd "$wt" && cargo bench -q -p eo-cache --bench open -- --save-baseline "$tag" --noplot)
echo "== current tree, change from $tag"
cargo bench -q -p eo-cache --bench open -- --baseline "$tag" --noplot
