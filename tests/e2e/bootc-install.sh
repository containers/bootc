#!/bin/bash
set -exuo pipefail

source ./shared_lib.sh
dump_runner
deploy_libvirt_network

ARCH=$(uname -m)

TEMPDIR=$(mktemp -d)
trap 'rm -rf -- "$TEMPDIR"' EXIT

# SSH configurations
SSH_KEY=${TEMPDIR}/id_rsa
ssh-keygen -f "${SSH_KEY}" -N "" -q -t rsa-sha2-256 -b 2048
SSH_KEY_PUB="${SSH_KEY}.pub"

INSTALL_CONTAINERFILE=${TEMPDIR}/Containerfile.install
UPGRADE_CONTAINERFILE=${TEMPDIR}/Containerfile.upgrade
QUAY_REPO_TAG="${QUAY_REPO_TAG:-$(tr -dc a-z0-9 < /dev/urandom | head -c 4 ; echo '')}"
INVENTORY_FILE="${TEMPDIR}/inventory"
# Local registry IP and port
REGISTRY_IP="192.168.100.1"
REGISTRY_PORT=5000

# VM firmware
if [[ "$ARCH" == "x86_64" ]]; then
    FIRMWARE_LIST=( \
        "bios" \
        "uefi" \
    )
    RND_LINE=$((RANDOM % 2))
    FIRMWARE="${FIRMWARE_LIST[$RND_LINE]}"
else
    FIRMWARE="uefi"
fi

# Get OS data.
source /etc/os-release

case ""${ID}-${VERSION_ID}"" in
    "centos-9")
        TEST_OS="centos-stream-9"
        TIER1_IMAGE_URL="quay.io/centos-bootc/centos-bootc:stream9"
        SSH_USER="cloud-user"
        REDHAT_VERSION_ID="9"
        BOOT_ARGS="uefi,firmware.feature0.name=secure-boot,firmware.feature0.enabled=no"
        ;;
    "centos-10")
        TEST_OS="centos-stream-10"
        TIER1_IMAGE_URL="quay.io/centos-bootc/centos-bootc:stream10"
        SSH_USER="cloud-user"
        REDHAT_VERSION_ID="10"
        BOOT_ARGS="uefi,firmware.feature0.name=secure-boot,firmware.feature0.enabled=no"
        # workaround CS10 libvirt selinux policy issue https://issues.redhat.com/browse/RHEL-46893
        sudo setenforce 0
        ;;
    "fedora-"*)
        TEST_OS="fedora-${VERSION_ID}"
        TIER1_IMAGE_URL="quay.io/fedora/fedora-bootc:${VERSION_ID}"
        REDHAT_VERSION_ID="${VERSION_ID}"
        SSH_USER="fedora"
        BOOT_ARGS="uefi"
        ;;
    *)
        redprint "Variable TEST_OS has to be defined"
        exit 1
        ;;
esac


# FIXME: https://github.com/containers/podman/issues/22813
if [[ "$REDHAT_VERSION_ID" == "10" ]]; then
    sed -i 's/^compression_format = .*/compression_format = "gzip"/' /usr/share/containers/containers.conf
fi

# Setup local registry
greenprint "Generate certificate"
openssl req \
    -newkey rsa:4096 \
    -nodes \
    -sha256 \
    -keyout "${TEMPDIR}/domain.key" \
    -addext "subjectAltName = IP:${REGISTRY_IP}" \
    -x509 \
    -days 365 \
    -out "${TEMPDIR}/domain.crt" \
    -subj "/C=US/ST=Denial/L=Stockholm/O=bootc/OU=bootc-test/CN=bootc-test/emailAddress=bootc-test@bootc-test.org"

greenprint "Update CA Trust"
sudo cp "${TEMPDIR}/domain.crt" "/etc/pki/ca-trust/source/anchors/${REGISTRY_IP}.crt"
sudo update-ca-trust

greenprint "Deploy local registry"
sudo podman run \
    -d \
    --name registry \
    --replace \
    --network host \
    -v "${TEMPDIR}":/certs:z \
    -e REGISTRY_HTTP_ADDR="${REGISTRY_IP}:${REGISTRY_PORT}" \
    -e REGISTRY_HTTP_TLS_CERTIFICATE=/certs/domain.crt \
    -e REGISTRY_HTTP_TLS_KEY=/certs/domain.key \
    quay.io/bootc-test/registry:2.8.3
sudo podman ps -a

# Test image URL
TEST_IMAGE_NAME="bootc-workflow-test"
TEST_IMAGE_URL="${REGISTRY_IP}:${REGISTRY_PORT}/${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}"

# Debug PACKIT_COPR_PROJECT and PACKIT_COPR_RPMS
echo "$PACKIT_COPR_PROJECT and $PACKIT_COPR_RPMS"

# Generate bootc copr repo file
if [[ "$VERSION_ID" == 41 ]]; then
    REPLACE_TEST_OS="${ID}-rawhide"
else
    REPLACE_TEST_OS="$TEST_OS"
fi
sed "s|REPLACE_COPR_PROJECT|${PACKIT_COPR_PROJECT}|; s|REPLACE_TEST_OS|${REPLACE_TEST_OS}|" files/bootc.repo.template | tee "${TEMPDIR}"/bootc.repo > /dev/null

# Configure continerfile
greenprint "Create $TEST_OS installation Containerfile"
tee "$INSTALL_CONTAINERFILE" > /dev/null << REALEOF
FROM "$TIER1_IMAGE_URL"
COPY bootc.repo /etc/yum.repos.d/
COPY domain.crt /etc/pki/ca-trust/source/anchors/
RUN dnf -y update bootc && \
    update-ca-trust
RUN cat <<EOF >> /usr/lib/bootc/install/00-mitigations.toml
[install.filesystem.root]
type = "xfs"
[install]
kargs = ["mitigations=on", "nosmt"]
EOF
RUN mkdir -p /usr/lib/bootc/kargs.d
RUN cat <<EOF >> /usr/lib/bootc/kargs.d/01-console.toml
kargs = ["console=ttyS0","panic=0"]
EOF
REALEOF

case "$TEST_CASE" in
    "to-existing-root")
        SSH_USER="root"
        SSH_KEY_PUB_CONTENT=$(cat "${SSH_KEY_PUB}")
        mkdir -p "${TEMPDIR}/usr/share/containers/systemd"
        cp files/caddy.container files/node_exporter.container "${TEMPDIR}/usr/share/containers/systemd"
        tee -a "$INSTALL_CONTAINERFILE" > /dev/null << EOF
COPY usr/ usr/
RUN mkdir -p /usr/etc-system/ && \
    echo 'AuthorizedKeysFile /usr/etc-system/%u.keys' >> /etc/ssh/sshd_config.d/30-auth-system.conf && \
    echo "$SSH_KEY_PUB_CONTENT" > /usr/etc-system/root.keys && \
    chmod 0600 /usr/etc-system/root.keys && \
    dnf -y install qemu-guest-agent && \
    dnf clean all && \
    systemctl enable qemu-guest-agent && \
    ln -s /usr/share/containers/systemd/caddy.container /usr/lib/bootc/bound-images.d/caddy.container && \
    ln -s /usr/share/containers/systemd/node_exporter.container /usr/lib/bootc/bound-images.d/node_exporter.container
EOF
    # logical bound image
    LBI="enabled"
    ;;
    "to-disk")
        tee -a "$INSTALL_CONTAINERFILE" > /dev/null << EOF
RUN dnf -y install python3 cloud-init && \
    dnf -y clean all
EOF
    # LBI is disabled in to-disk test
    LBI="disabled"
    ;;
esac

greenprint "Check $TEST_OS installation Containerfile"
cat "$INSTALL_CONTAINERFILE"

# Build test bootc image and push to local registry
greenprint "Build $TEST_OS installation container image"
sudo podman build --tls-verify=false -t "${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}" -f "$INSTALL_CONTAINERFILE" "$TEMPDIR"

greenprint "Push $TEST_OS installation container image"
sudo podman push --tls-verify=false --quiet "${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}" "$TEST_IMAGE_URL"

# Prepare Ansible inventory file and ansible.cfg
greenprint "Prepare inventory file"
tee -a "$INVENTORY_FILE" > /dev/null << EOF
[cloud]
localhost

[guest]

[cloud:vars]
ansible_connection=local

[guest:vars]
ansible_user="$SSH_USER"
ansible_private_key_file="$SSH_KEY"
ansible_ssh_common_args="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null"

[all:vars]
ansible_python_interpreter=/usr/bin/python3
EOF

greenprint "Prepare ansible.cfg"
export ANSIBLE_CONFIG="playbooks/ansible.cfg"

# Run bootc install to-disk test
case "$TEST_CASE" in
    "to-existing-root")
        DOWNLOAD_IMAGE="true"
        AIR_GAPPED_DIR="$TEMPDIR"/virtiofs
        mkdir "$AIR_GAPPED_DIR"
    ;;
    "to-disk")
        DOWNLOAD_IMAGE="false"
        AIR_GAPPED_DIR=""
        greenprint "Configure rootfs randomly"
        ROOTFS_LIST=( \
            "ext4" \
            "xfs" \
        )
        RND_LINE=$((RANDOM % 2))
        ROOTFS="${ROOTFS_LIST[$RND_LINE]}"

        if [[ "$TEST_OS" == "fedora"* ]]; then
            ROOTFS="btrfs"
        fi

        greenprint "💾 Create disk.raw"
        sudo truncate -s 10G disk.raw

        greenprint "bootc install to disk.raw"
        sudo podman run \
            --rm \
            --privileged \
            --pid=host \
            --security-opt label=type:unconfined_t \
            -v .:/output \
            "$TEST_IMAGE_URL" \
            bootc install to-disk --filesystem "$ROOTFS" --generic-image --via-loopback /output/disk.raw

        sudo qemu-img convert -f raw ./disk.raw -O qcow2 "/var/lib/libvirt/images/disk.qcow2"
        rm -f disk.raw
    ;;
esac

# Start disk.qcow for to-disk test
# Start a new VM for to-existing-root test
greenprint "Deploy VM"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e test_os="$TEST_OS" \
    -e ssh_user="$SSH_USER" \
    -e ssh_key_pub="$SSH_KEY_PUB" \
    -e inventory_file="$INVENTORY_FILE" \
    -e download_image="$DOWNLOAD_IMAGE" \
    -e air_gapped_dir="$AIR_GAPPED_DIR" \
    -e firmware="$FIRMWARE" \
    -e boot_args="$BOOT_ARGS" \
    playbooks/deploy-libvirt.yaml

# Run bootc install to-existing-root test
if [[ "$TEST_CASE" == "to-existing-root" ]]; then
    greenprint "Install $TEST_OS bootc system"
    ansible-playbook -v \
        -i "$INVENTORY_FILE" \
        -e test_os="$TEST_OS" \
        -e test_image_url="$TEST_IMAGE_URL" \
        -e test_case="$TEST_CASE" \
        playbooks/install.yaml
fi

# Check bootc system
greenprint "Run ostree checking test on VM"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e test_os="$TEST_OS" \
    -e bootc_image="$TEST_IMAGE_URL" \
    -e image_label_version_id="$REDHAT_VERSION_ID" \
    -e kargs="mitigations=on,nosmt,console=ttyS0,panic=0" \
    -e lbi="$LBI" \
    playbooks/check-system.yaml

# Prepare upgrade containerfile
greenprint "Create upgrade Containerfile"
tee "$UPGRADE_CONTAINERFILE" > /dev/null << REALEOF
FROM "$TEST_IMAGE_URL"
RUN dnf -y install wget && \
    dnf -y clean all
RUN rm /usr/lib/bootc/kargs.d/01-console.toml
RUN cat <<EOF >> /usr/lib/bootc/kargs.d/01-console.toml
kargs = ["systemd.unified_cgroup_hierarchy=1","console=ttyS","panic=0"]
EOF
REALEOF

# Build upgrade container image and push to locay registry
greenprint "Build $TEST_OS upgrade container image"
sudo podman build --tls-verify=false -t "${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}" -f "$UPGRADE_CONTAINERFILE" .

greenprint "Push $TEST_OS upgrade container image"
sudo podman push --tls-verify=false --quiet "${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}" "$TEST_IMAGE_URL"

# Copy upgrade image to local folder for bootc switch test
if [[ "$AIR_GAPPED_DIR" != "" ]]; then
    skopeo copy docker://"$TEST_IMAGE_URL" dir://"$AIR_GAPPED_DIR"
    BOOTC_IMAGE="/mnt"
else
    BOOTC_IMAGE="$TEST_IMAGE_URL"
fi

# bootc upgrade/switch test
greenprint "Upgrade $TEST_OS system"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e air_gapped_dir="$AIR_GAPPED_DIR" \
    playbooks/upgrade.yaml

# Check bootc system after upgrade/switch
greenprint "Run ostree checking test after upgrade on VM"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e test_os="$TEST_OS" \
    -e bootc_image="$BOOTC_IMAGE" \
    -e image_label_version_id="$REDHAT_VERSION_ID" \
    -e upgrade="true" \
    -e kargs="systemd.unified_cgroup_hierarchy=1,console=ttyS,panic=0" \
    -e lbi="$LBI" \
    playbooks/check-system.yaml

# bootc rollback test
greenprint "Rollback $TEST_OS system"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e air_gapped_dir="$AIR_GAPPED_DIR" \
    playbooks/rollback.yaml

# Test finished and system clean up
greenprint "Clean up"
unset ANSIBLE_CONFIG
sudo virsh destroy "bootc-${TEST_OS}"
if [[ "$FIRMWARE" == "uefi" ]]; then
    sudo virsh undefine "bootc-${TEST_OS}" --nvram
else
    sudo virsh undefine "bootc-${TEST_OS}"
fi
if [[ "$TEST_CASE" == "to-disk" ]]; then
    sudo virsh vol-delete --pool images disk.qcow2
else
    sudo virsh vol-delete --pool images "bootc-${TEST_OS}.qcow2"
fi

greenprint "🎉 All tests passed."
exit 0
