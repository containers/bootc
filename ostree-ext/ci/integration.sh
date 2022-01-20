#!/bin/bash
# Assumes that the current environment is a mutable ostree-container
# with ostree-ext-cli installed in /usr/bin.  
# Runs integration tests.
set -xeuo pipefail

# Output an ok message for TAP
n_tap_tests=0
tap_ok() {
    echo "ok" "$@"
    n_tap_tests=$(($n_tap_tests+1))
}

tap_end() {
    echo "1..${n_tap_tests}"
}

env=$(ostree-ext-cli internal-only-for-testing detect-env)
test "${env}" = ostree-container
tap_ok environment

ostree-ext-cli internal-only-for-testing run
tap_ok integrationtests

tap_end
