#!/bin/bash

# Dumps details about the instance running the CI job.
function dump_runner {
    RUNNER_CPUS=$(nproc)
    RUNNER_MEM=$(free -m | grep -oP '\d+' | head -n 1)
    RUNNER_DISK=$(df --output=size -h / | sed '1d;s/[^0-9]//g')
    RUNNER_HOSTNAME=$(uname -n)
    RUNNER_USER=$(whoami)
    RUNNER_ARCH=$(uname -m)
    RUNNER_KERNEL=$(uname -r)

    echo -e "\033[0;36m"
    cat << EOF
------------------------------------------------------------------------------
CI MACHINE SPECS
------------------------------------------------------------------------------
    Hostname: ${RUNNER_HOSTNAME}
        User: ${RUNNER_USER}
        CPUs: ${RUNNER_CPUS}
         RAM: ${RUNNER_MEM} MB
        DISK: ${RUNNER_DISK} GB
        ARCH: ${RUNNER_ARCH}
      KERNEL: ${RUNNER_KERNEL}
------------------------------------------------------------------------------
EOF
}

# Colorful timestamped output.
function greenprint {
    echo -e "\033[1;32m[$(date -Isecond)] ${1}\033[0m"
}

function redprint {
    echo -e "\033[1;31m[$(date -Isecond)] ${1}\033[0m"
}

function deploy_libvirt_network {
    greenprint "Start firewalld"
    sudo systemctl enable --now firewalld

    greenprint "🚀 Starting libvirt daemon"
    sudo systemctl start libvirtd
    sudo virsh list --all > /dev/null

    # Set a customized dnsmasq configuration for libvirt so we always get the
    # same address on boot up.
    greenprint "💡 Setup libvirt network"
    sudo tee /tmp/integration.xml > /dev/null << EOF
<network xmlns:dnsmasq='http://libvirt.org/schemas/network/dnsmasq/1.0'>
<name>integration</name>
<uuid>1c8fe98c-b53a-4ca4-bbdb-deb0f26b3579</uuid>
<forward mode='nat'>
    <nat>
    <port start='1024' end='65535'/>
    </nat>
</forward>
<bridge name='integration' zone='trusted' stp='on' delay='0'/>
<mac address='52:54:00:36:46:ef'/>
<ip address='192.168.100.1' netmask='255.255.255.0'>
    <dhcp>
    <range start='192.168.100.2' end='192.168.100.254'/>
    <host mac='34:49:22:B0:83:30' name='vm-1' ip='192.168.100.50'/>
    <host mac='34:49:22:B0:83:31' name='vm-2' ip='192.168.100.51'/>
    <host mac='34:49:22:B0:83:32' name='vm-3' ip='192.168.100.52'/>
    </dhcp>
</ip>
<dnsmasq:options>
    <dnsmasq:option value='dhcp-vendorclass=set:efi-http,HTTPClient:Arch:00016'/>
    <dnsmasq:option value='dhcp-option-force=tag:efi-http,60,HTTPClient'/>
    <dnsmasq:option value='dhcp-boot=tag:efi-http,&quot;http://192.168.100.1/httpboot/EFI/BOOT/BOOTX64.EFI&quot;'/>
</dnsmasq:options>
</network>
EOF
    if ! sudo virsh net-info integration > /dev/null 2>&1; then
        sudo virsh net-define /tmp/integration.xml
    fi
    if [[ $(sudo virsh net-info integration | grep 'Active' | awk '{print $2}') == 'no' ]]; then
        sudo virsh net-start integration
    fi
    sudo rm -f /tmp/integration.xml
}
