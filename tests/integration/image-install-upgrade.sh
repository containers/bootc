#!/bin/bash
set -exuo pipefail

source ./shared_lib.sh
dump_runner

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

TEST_IMAGE_NAME="bootc-workflow-test"

case "$TEST_OS" in
    "rhel-9"*)
        if [[ "$TEST_OS" == "rhel-9-4" ]]; then
            TIER1_IMAGE_URL="${RHEL_REGISTRY_URL}/rhel9-rhel_bootc:rhel-9.4"
            BATCH_COMPOSE="updates/"
            LATEST_COMPOSE_ID="latest-RHEL-9.4.0"
            REDHAT_VERSION_ID="9.4"
        else
            TIER1_IMAGE_URL="${RHEL_REGISTRY_URL}/rhel9-rhel_bootc:rhel-9.5"
            BATCH_COMPOSE=""
            LATEST_COMPOSE_ID="latest-RHEL-9.5.0"
            REDHAT_VERSION_ID="9.5"
        fi
        TEST_IMAGE_URL="quay.io/redhat_emp1/${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}"
        SSH_USER="cloud-user"
        sed "s/REPLACE_ME/${DOWNLOAD_NODE}/; s|REPLACE_BATCH_COMPOSE|${BATCH_COMPOSE}|; s/REPLACE_COMPOSE_ID/${LATEST_COMPOSE_ID}/" files/rhel-9-y.template | tee rhel-9-y.repo > /dev/null
        ADD_REPO="COPY rhel-9-y.repo /etc/yum.repos.d/rhel-9-y.repo"
        greenprint "Prepare cloud-init file"
        tee -a "playbooks/user-data" > /dev/null << EOF
#cloud-config
yum_repos:
  rhel-9y-baseos:
    name: rhel-9y-baseos
    baseurl: http://${DOWNLOAD_NODE}/rhel-9/nightly/${BATCH_COMPOSE}RHEL-9/${LATEST_COMPOSE_ID}/compose/BaseOS/\$basearch/os/
    enabled: true
    gpgcheck: false
  rhel-9y-appstream:
    name: rhel-9y-appstream
    baseurl: http://${DOWNLOAD_NODE}/rhel-9/nightly/${BATCH_COMPOSE}RHEL-9/${LATEST_COMPOSE_ID}/compose/AppStream/\$basearch/os/
    enabled: true
    gpgcheck: false
EOF
        BOOT_ARGS="uefi"
        ;;
    "centos-stream-9")
        TIER1_IMAGE_URL="quay.io/centos-bootc/centos-bootc-dev:stream9"
        ADD_REPO=""
        SSH_USER="cloud-user"
        REDHAT_VERSION_ID="9"
        TEST_IMAGE_URL="quay.io/bootc-test/${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}"
        BOOT_ARGS="uefi,firmware.feature0.name=secure-boot,firmware.feature0.enabled=no"
        ;;
    "fedora"*)
        if [[ "$TEST_OS" == "fedora-40" ]]; then
            TIER1_IMAGE_URL="quay.io/fedora/fedora-bootc:40"
            REDHAT_VERSION_ID="40"
        else
            TIER1_IMAGE_URL="quay.io/fedora/fedora-bootc:41"
            REDHAT_VERSION_ID="41"
        fi
        SSH_USER="fedora"
        ADD_REPO=""
        TEST_IMAGE_URL="quay.io/bootc-test/${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}"
        BOOT_ARGS="uefi"
        ;;
    *)
        redprint "Variable TIER1_IMAGE_URL is not supported"
        exit 1
        ;;
esac

sed "s/REPLACE_ME/${QUAY_SECRET}/g" files/auth.template | tee auth.json > /dev/null
greenprint "Create $TEST_OS installation Containerfile"
tee "$INSTALL_CONTAINERFILE" > /dev/null << REALEOF
FROM "$TIER1_IMAGE_URL"
$ADD_REPO
COPY build/bootc-2*.${ARCH}.rpm .
RUN dnf -y update ./bootc-2*.${ARCH}.rpm && \
    rm -f ./bootc-2*.${ARCH}.rpm
RUN dnf -y install python3 cloud-init && \
    dnf -y clean all
COPY auth.json /etc/ostree/auth.json
RUN cat <<EOF >> /usr/lib/bootc/install/00-mitigations.toml
[install.filesystem.root]
type = "xfs"
[install]
kargs = ["mitigations=on"]
EOF
REALEOF

greenprint "Check $TEST_OS installation Containerfile"
cat "$INSTALL_CONTAINERFILE"

greenprint "Login quay.io"
sudo podman login -u "${QUAY_USERNAME}" -p "${QUAY_PASSWORD}" quay.io

greenprint "Build $TEST_OS installation container image"
sudo podman build --tls-verify=false --retry=5 --retry-delay=10 -t "${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}" -f "$INSTALL_CONTAINERFILE" .

greenprint "Push $TEST_OS installation container image"
sudo podman push --tls-verify=false --quiet "${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}" "$TEST_IMAGE_URL"

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
export ANSIBLE_CONFIG="${PWD}/playbooks/ansible.cfg"

case "$IMAGE_TYPE" in
    "to-disk")
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
            -v /dev:/dev \
            -v /var/lib/containers:/var/lib/containers \
            -v /dev:/dev \
            -v .:/output \
            "$TEST_IMAGE_URL" \
            bootc install to-disk --filesystem "$ROOTFS" --generic-image --via-loopback /output/disk.raw

        sudo qemu-img convert -f raw ./disk.raw -O qcow2 "/var/lib/libvirt/images/disk.qcow2"
        rm -f disk.raw

        if [[ "$ARCH" == "x86_64" ]]; then
            BIB_FIRMWARE_LIST=( \
                "bios" \
                "uefi" \
            )
            RND_LINE=$((RANDOM % 2))
            BIB_FIRMWARE="${BIB_FIRMWARE_LIST[$RND_LINE]}"
        else
            BIB_FIRMWARE="uefi"
        fi

        greenprint "Deploy $IMAGE_TYPE instance"
        ansible-playbook -v \
            -i "$INVENTORY_FILE" \
            -e test_os="$TEST_OS" \
            -e ssh_key_pub="$SSH_KEY_PUB" \
            -e ssh_user="$SSH_USER" \
            -e inventory_file="$INVENTORY_FILE" \
            -e bib="true" \
            -e boot_args="$BOOT_ARGS" \
            -e bib_firmware="$BIB_FIRMWARE" \
            "playbooks/deploy-libvirt.yaml"
        ;;
    *)
        redprint "Variable IMAGE_TYPE has to be defined"
        exit 1
        ;;
esac

greenprint "Run ostree checking test on $PLATFORM instance"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e test_os="$TEST_OS" \
    -e bootc_image="$TEST_IMAGE_URL" \
    -e image_label_version_id="$REDHAT_VERSION_ID" \
    -e kargs="mitigations=on,nosmt" \
    playbooks/check-system.yaml

greenprint "Create upgrade Containerfile"
tee "$UPGRADE_CONTAINERFILE" > /dev/null << REALEOF
FROM "$TEST_IMAGE_URL"
RUN dnf -y install wget && \
    dnf -y clean all
RUN cat <<EOF >> /usr/lib/bootc/kargs.d/01-console.toml
kargs = ["systemd.unified_cgroup_hierarchy=0"]
EOF
REALEOF

greenprint "Build $TEST_OS upgrade container image"
sudo podman build --tls-verify=false --retry=5 --retry-delay=10 -t "${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}" -f "$UPGRADE_CONTAINERFILE" .
greenprint "Push $TEST_OS upgrade container image"
sudo podman push --tls-verify=false --quiet "${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}" "$TEST_IMAGE_URL"

greenprint "Upgrade $TEST_OS system"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    playbooks/upgrade.yaml

greenprint "Run ostree checking test after upgrade on $PLATFORM instance"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e test_os="$TEST_OS" \
    -e bootc_image="$TEST_IMAGE_URL" \
    -e image_label_version_id="$REDHAT_VERSION_ID" \
    -e upgrade="true" \
    -e kargs="systemd.unified_cgroup_hierarchy=0" \
    playbooks/check-system.yaml

greenprint "Create second upgrade Containerfile to test kargs delta"
tee "$UPGRADE_CONTAINERFILE" > /dev/null << REALEOF
FROM "$TEST_IMAGE_URL"
RUN dnf -y install wget && \
    dnf -y clean all
RUN cat <<EOF >> /usr/lib/bootc/kargs.d/01-console.toml
kargs = ["systemd.unified_cgroup_hierarchy=1"]
EOF
REALEOF

greenprint "Upgrade $TEST_OS system"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    playbooks/upgrade.yaml

greenprint "Run ostree checking test after upgrade on $PLATFORM instance"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e test_os="$TEST_OS" \
    -e bootc_image="$TEST_IMAGE_URL" \
    -e image_label_version_id="$REDHAT_VERSION_ID" \
    -e upgrade="true" \
    -e kargs="systemd.unified_cgroup_hierarchy=1" \
    playbooks/check-system.yaml

greenprint "Rollback $TEST_OS system"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    playbooks/rollback.yaml

greenprint "Terminate $PLATFORM instance and deregister AMI"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e platform="$PLATFORM" \
    playbooks/remove.yaml

greenprint "Clean up"
rm -rf auth.json rhel-9-y.repo
unset ANSIBLE_CONFIG

greenprint "🎉 All tests passed."
exit 0
