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
rm vendor ${vendor_dest} -rf
cargo vendor
# Trim off Windows pre-built libraries that make the vendor/ dir much larger
find vendor/ -name '*.a' -delete
tar czvf ${vendor_dest}.tmp vendor/
rm vendor -rf
mv -Tf ${vendor_dest}{.tmp,}

echo "Prepared ${version} at commit ${commit}"
