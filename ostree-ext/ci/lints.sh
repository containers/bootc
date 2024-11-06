#!/usr/bin/env bash
set -xeuo pipefail
tmpf=$(mktemp)
git grep 'dbg!' '*.rs' > ${tmpf} || true
if test -s ${tmpf}; then
    echo "Found dbg!" 1>&2
    cat "${tmpf}"
    exit 1
fi