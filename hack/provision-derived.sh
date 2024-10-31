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

      # tmt puts scrips in /var/lib/tmt/scripts, add them to $PATH
      touch /etc/environment
      echo "export PATH=$PATH:/var/lib/tmt/scripts" >> /etc/environment

      # tmt needs a webserver to verify the VM is running
      TESTCLOUD_GUEST="python3 -m http.server 10022 || python -m http.server 10022 || /usr/libexec/platform-python -m http.server 10022 || python2 -m SimpleHTTPServer 10022 || python -m SimpleHTTPServer 10022"
      echo "$TESTCLOUD_GUEST" >> /opt/testcloud-guest.sh
      chmod +x /opt/testcloud-guest.sh
      echo "[Unit]" >> /etc/systemd/system/testcloud.service
      echo "Description=Testcloud guest integration" >> /etc/systemd/system/testcloud.service
      echo "After=cloud-init.service" >> /etc/systemd/system/testcloud.service
      echo "[Service]" >> /etc/systemd/system/testcloud.service
      echo "ExecStart=/bin/bash /opt/testcloud-guest.sh" >> /etc/systemd/system/testcloud.service
      echo "[Install]" >> /etc/systemd/system/testcloud.service
      echo "WantedBy=multi-user.target" >> /etc/systemd/system/testcloud.service
      systemctl enable testcloud.service
      ;;
    "") echo "No variant" 
      ;;
    *) 
      echo "Unknown variant: $1" exit 1
      ;;
esac
dnf clean all && rm /var/log/* -rf
