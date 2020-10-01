# Source library for shell script tests
# Copyright (C) 2020 Red Hat, Inc.
# SPDX-License-Identifier: Apache-2.0

runv() {
    (set -x && "$@")
}

N_TESTS=0
ok() {
    echo "ok" $@
    N_TESTS=$((N_TESTS + 1))
}

tap_finish() {
    echo "Completing TAP test with:"
    echo "1..${N_TESTS}"
}

fatal() {
    echo error: $@ 1>&2; exit 1
}

runv() {
    set -x
    "$@"
}

# Dump ls -al + file contents to stderr, then fatal()
_fatal_print_file() {
    file="$1"
    shift
    ls -al "$file" >&2
    sed -e 's/^/# /' < "$file" >&2
    fatal "$@"
}

assert_not_has_file () {
    fpath=$1
    shift
    if test -e "$fpath"; then
        fatal "Path exists: ${fpath}"
    fi
}

assert_file_has_content () {
    fpath=$1
    shift
    for re in "$@"; do
        if ! grep -q -e "$re" "$fpath"; then
            _fatal_print_file "$fpath" "File '$fpath' doesn't match regexp '$re'"
        fi
    done
}

assert_file_has_content_literal () {
    fpath=$1; shift
    for s in "$@"; do
        if ! grep -q -F -e "$s" "$fpath"; then
            _fatal_print_file "$fpath" "File '$fpath' doesn't match fixed string list '$s'"
        fi
    done
}

assert_not_file_has_content () {
    fpath=$1
    shift
    for re in "$@"; do
        if grep -q -e "$re" "$fpath"; then
            _fatal_print_file "$fpath" "File '$fpath' matches regexp '$re'"
        fi
    done
}

assert_not_file_has_content_literal () {
    fpath=$1; shift
    for s in "$@"; do
        if grep -q -F -e "$s" "$fpath"; then
            _fatal_print_file "$fpath" "File '$fpath' matches fixed string list '$s'"
        fi
    done
}

