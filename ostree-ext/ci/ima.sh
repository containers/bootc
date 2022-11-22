#!/bin/bash
# Assumes that the current environment is a mutable ostree-container
# with ostree-ext-cli installed in /usr/bin.  
# Runs IMA tests.
set -xeuo pipefail

# https://github.com/ostreedev/ostree-rs-ext/issues/417
mkdir -p /var/tmp

if test '!' -x /usr/bin/evmctl; then
    rpm-ostree install ima-evm-utils
fi

ostree-ext-cli internal-only-for-testing run-ima
echo ok "ima"
