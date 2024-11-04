#!/bin/bash
# Prepare a release
set -euo pipefail
cargo publish --dry-run
name=$(cargo read-manifest | jq -r .name)
version=$(cargo read-manifest | jq -r .version)
commit=$(git rev-parse HEAD)

# Generate a vendor tarball of sources to attach to a release
# in order to support offline builds.
vendor_dest=target/${name}-${version}-vendor.tar.gz
cargo vendor-filterer --prefix=vendor --format=tar.gz "${vendor_dest}"

echo "Prepared ${version} at commit ${commit}"
