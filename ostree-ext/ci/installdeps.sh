#!/bin/bash
set -xeuo pipefail

# Always pull ostree from updates-testing to avoid the bodhi wait
dnf -y --enablerepo=updates-testing update ostree-devel

# Our tests depend on this
dnf -y install skopeo
