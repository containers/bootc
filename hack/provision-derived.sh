#!/bin/bash
set -xeu
case "$1" in
    tmt)
      # tmt wants rsync
      dnf -y install cloud-init rsync && dnf clean all
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
