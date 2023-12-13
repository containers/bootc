#!/bin/bash
set -xeuo pipefail

OS_ID=$(. /usr/lib/os-release && echo $ID)

baseurl=
case $OS_ID in
    fedora) baseurl=https://download.copr.fedorainfracloud.org/results/@CoreOS/continuous/fedora-\$releasever-\$basearch/ ;;
    # Default to c9s (also covers all variants/derivatives)
    *) baseurl=https://download.copr.fedorainfracloud.org/results/@CoreOS/continuous/centos-stream-\$releasever-\$basearch/ ;;
esac

# For some reason dnf copr enable -y says there are no builds?
cat >/etc/yum.repos.d/coreos-continuous.repo << EOF
[copr:copr.fedorainfracloud.org:group_CoreOS:continuous]
name=Copr repo for continuous owned by @CoreOS
baseurl=$baseurl
type=rpm-md
skip_if_unavailable=True
gpgcheck=1
gpgkey=https://download.copr.fedorainfracloud.org/results/@CoreOS/continuous/pubkey.gpg
repo_gpgcheck=0
enabled=1
enabled_metadata=1
EOF

# Our tests depend on this
dnf -y install skopeo zstd

case $OS_ID in
    fedora) dnf -y builddep bootc ;;
    *) dnf -y install openssl-devel ostree-devel cargo ;;
esac
