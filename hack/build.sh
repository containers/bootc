#!/bin/bash
set -xeu
. /usr/lib/os-release
case $ID in
  centos|rhel) dnf config-manager --set-enabled crb;;
  fedora) dnf -y install dnf-utils ;;
esac
# Fetch the latest spec from fedora to ensure we've got the latest build deps
t=$(mktemp --suffix .spec)
curl -L -o ${t} https://src.fedoraproject.org/rpms/bootc/raw/rawhide/f/bootc.spec
dnf -y builddep "${t}"
rm -f "${t}"
# Extra dependencies
dnf -y install git-core
