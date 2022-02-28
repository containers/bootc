#!/bin/bash
set -xeuo pipefail

# Always pull ostree from updates-testing to avoid the bodhi wait
dnf -y --enablerepo=updates-testing update ostree-devel

# Our tests depend on this
dnf -y install skopeo

# For some reason dnf copr enable -y says there are no builds?
cat >/etc/yum.repos.d/coreos-continuous.repo << 'EOF'
[copr:copr.fedorainfracloud.org:group_CoreOS:continuous]
name=Copr repo for continuous owned by @CoreOS
baseurl=https://download.copr.fedorainfracloud.org/results/@CoreOS/continuous/fedora-$releasever-$basearch/
type=rpm-md
skip_if_unavailable=True
gpgcheck=1
gpgkey=https://download.copr.fedorainfracloud.org/results/@CoreOS/continuous/pubkey.gpg
repo_gpgcheck=0
enabled=1
enabled_metadata=1
EOF

# For now pull ostree from git
dnf update -y ostree
