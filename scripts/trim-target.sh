#!/usr/bin/env bash
# Keeps target/ small: when it grows past LIMIT_GB (default 15), drops the
# dev build and the incremental caches. Run it after big build steps.
#   usage: scripts/trim-target.sh
#   env:   LIMIT_GB=15 size in GB past which target/debug is deleted
set -euo pipefail
LIMIT=${LIMIT_GB:-15}
[ -d target ] || { echo "no target/ yet"; exit 0; }
du -sh target
KB=$(du -sk target | cut -f1)
if [ "$KB" -gt $((LIMIT * 1024 * 1024)) ]; then
    rm -rf target/debug target/*/incremental
    echo "over ${LIMIT} GB: dropped target/debug and the incremental caches"
    du -sh target
fi
