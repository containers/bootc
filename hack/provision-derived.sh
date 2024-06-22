#!/bin/bash
set -xeu
variant=$1
# I'm a big fan of nushell for interactive use, and I want to support
# using it in our test suite because it's better than bash. First,
# enable EPEL to get it.
. /usr/lib/os-release
if echo $ID_LIKE $ID | grep -q centos; then
  dnf config-manager --set-enabled crb
  dnf -y install epel-release epel-next-release
fi
# Ensure this is pre-created
mkdir -p -m 0700 /var/roothome
mkdir -p ~/.config/nushell
echo '$env.config = { show_banner: false, }' > ~/.config/nushell/config.nu
touch ~/.config/nushell/env.nu
dnf -y install nu
# And we also add pytest, to support running tests written in Python
dnf -y install python3-pytest
case "$variant" in
    tmt)
      # tmt wants rsync
      dnf -y install cloud-init rsync
      ln -s ../cloud-init.target /usr/lib/systemd/system/default.target.wants
      # And tmt wants to write to /usr/local/bin
      rm /usr/local -rf && ln -sr /var/usrlocal /usr/local && mkdir -p /var/usrlocal/bin
      ;;
    "") echo "No variant" 
      ;;
    *) 
      echo "Unknown variant: $1" exit 1
      ;;
esac
dnf clean all && rm /var/log/* -rf
