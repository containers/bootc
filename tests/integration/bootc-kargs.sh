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

REPLACE_CLOUD_USER=""
TEST_IMAGE_NAME="bootc-workflow-test"
TEST_IMAGE_URL="quay.io/redhat_emp1/${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}"

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
        SSH_USER="cloud-user"
        sed "s/REPLACE_ME/${DOWNLOAD_NODE}/; s|REPLACE_BATCH_COMPOSE|${BATCH_COMPOSE}|; s/REPLACE_COMPOSE_ID/${LATEST_COMPOSE_ID}/" files/rhel-9-y.template | tee rhel-9-y.repo > /dev/null
        ADD_REPO="COPY rhel-9-y.repo /etc/yum.repos.d/rhel-9-y.repo"
        if [[ "$PLATFORM" == "aws" ]]; then
            SSH_USER="ec2-user"
            REPLACE_CLOUD_USER='RUN sed -i "s/name: cloud-user/name: ec2-user/g" /etc/cloud/cloud.cfg'
        fi
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
        ;;
    "centos-stream-9")
        TIER1_IMAGE_URL="quay.io/centos-bootc/centos-bootc-dev:stream9"
        SSH_USER="cloud-user"
        ADD_REPO=""
        if [[ "$PLATFORM" == "aws" ]]; then
            SSH_USER="ec2-user"
            REPLACE_CLOUD_USER='RUN sed -i "s/name: cloud-user/name: ec2-user/g" /etc/cloud/cloud.cfg'
        fi
        REDHAT_VERSION_ID="9"
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
        ;;
    *)
        redprint "Variable TEST_OS has to be defined"
        exit 1
        ;;
esac

sed "s/REPLACE_ME/${QUAY_SECRET}/g" files/auth.template | tee auth.json > /dev/null

greenprint "Create $TEST_OS installation Containerfile"
tee "$INSTALL_CONTAINERFILE" > /dev/null << REALEND
FROM "$TIER1_IMAGE_URL"
$ADD_REPO
COPY build/bootc-2*.${ARCH}.rpm .
RUN dnf -y update ./bootc-2*.${ARCH}.rpm && \
    rm -f ./bootc-2*.${ARCH}.rpm
COPY auth.json /etc/ostree/auth.json
RUN cat <<EOF >> /usr/lib/bootc/install/00-nosmt.toml
kargs = ["nosmt"]
EOF
REALEND

case "$PLATFORM" in
    "aws")
        tee -a "$INSTALL_CONTAINERFILE" > /dev/null << EOF
RUN dnf -y install python3 cloud-init && \
    dnf -y clean all
$REPLACE_CLOUD_USER
EOF
        ;;
    "libvirt")
        SSH_USER="root"
        SSH_KEY_PUB_CONTENT=$(cat "${SSH_KEY_PUB}")
        tee -a "$INSTALL_CONTAINERFILE" > /dev/null << EOF
RUN mkdir -p /usr/etc-system/ && \
    echo 'AuthorizedKeysFile /usr/etc-system/%u.keys' >> /etc/ssh/sshd_config.d/30-auth-system.conf && \
    echo "$SSH_KEY_PUB_CONTENT" > /usr/etc-system/root.keys && \
    chmod 0600 /usr/etc-system/root.keys && \
    dnf -y install qemu-guest-agent && \
    dnf clean all && \
    systemctl enable qemu-guest-agent
EOF
        ;;
esac

greenprint "Check $TEST_OS installation Containerfile"
cat "$INSTALL_CONTAINERFILE"

greenprint "Login quay.io"
podman login -u "${QUAY_USERNAME}" -p "${QUAY_PASSWORD}" quay.io

greenprint "Build $TEST_OS installation container image"
podman build --tls-verify=false --retry=5 --retry-delay=10 -t "${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}" -f "$INSTALL_CONTAINERFILE" .

greenprint "Push $TEST_OS installation container image"
retry podman push --tls-verify=false --quiet "${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}" "$TEST_IMAGE_URL"

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

# AIR_GAPPED=1 means add passthough mount to test bootc swtich to local disk
if [[ ${AIR_GAPPED-} -eq 1 ]];then
    AIR_GAPPED_DIR="$TEMPDIR"/virtiofs
    mkdir "$AIR_GAPPED_DIR"
else
    AIR_GAPPED=0
    AIR_GAPPED_DIR=""
fi

greenprint "Deploy $PLATFORM instance"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e test_os="$TEST_OS" \
    -e ssh_user="$SSH_USER" \
    -e ssh_key_pub="$SSH_KEY_PUB" \
    -e inventory_file="$INVENTORY_FILE" \
    -e air_gapped_dir="$AIR_GAPPED_DIR" \
    "playbooks/deploy-${PLATFORM}.yaml"

greenprint "Install $TEST_OS bootc system"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e test_os="$TEST_OS" \
    -e test_image_url="$TEST_IMAGE_URL" \
    playbooks/install.yaml

greenprint "Run ostree checking test on $PLATFORM instance"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e test_os="$TEST_OS" \
    -e bootc_image="$TEST_IMAGE_URL" \
    -e image_label_version_id="$REDHAT_VERSION_ID" \
    playbooks/check-system.yaml

greenprint "Run kargs install test on $PLATFORM instance"
ansible-playbook -v \
    -i "$INVENTORY_FILE" \
    -e test_os="$TEST_OS" \
    -e kargs="nosmt" \
    -e image_label_version_id="$REDHAT_VERSION_ID" \
    playbooks/check-system.yaml

# greenprint "Create upgrade Containerfile"
# tee "$UPGRADE_CONTAINERFILE" > /dev/null << EOF
# FROM "$TEST_IMAGE_URL"
# RUN dnf -y install wget && \
#     dnf -y clean all
# EOF

# greenprint "Build $TEST_OS upgrade container image"
# podman build --tls-verify=false --retry=5 --retry-delay=10 -t "${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}" -f "$UPGRADE_CONTAINERFILE" .

# greenprint "Push $TEST_OS upgrade container image"
# retry podman push --tls-verify=false --quiet "${TEST_IMAGE_NAME}:${QUAY_REPO_TAG}" "$TEST_IMAGE_URL"

# if [[ ${AIR_GAPPED-} -eq 1 ]]; then
#     retry skopeo copy docker://"$TEST_IMAGE_URL" dir://"$AIR_GAPPED_DIR"
#     BOOTC_IMAGE="/mnt"
# else
#     BOOTC_IMAGE="$TEST_IMAGE_URL"
# fi

# greenprint "Upgrade $TEST_OS system"
# ansible-playbook -v \
#     -i "$INVENTORY_FILE" \
#     -e air_gapped_dir="$AIR_GAPPED_DIR" \
#     playbooks/upgrade.yaml

# greenprint "Run ostree checking test after upgrade on $PLATFORM instance"
# ansible-playbook -v \
#     -i "$INVENTORY_FILE" \
#     -e test_os="$TEST_OS" \
#     -e bootc_image="$BOOTC_IMAGE" \
#     -e image_label_version_id="$REDHAT_VERSION_ID" \
#     -e upgrade="true" \
#     playbooks/check-system.yaml

# greenprint "Rollback $TEST_OS system"
# ansible-playbook -v \
#     -i "$INVENTORY_FILE" \
#     -e air_gapped_dir="$AIR_GAPPED_DIR" \
#     playbooks/rollback.yaml

# greenprint "Remove $PLATFORM instance"
# ansible-playbook -v \
#     -i "$INVENTORY_FILE" \
#     -e platform="$PLATFORM" \
#     playbooks/remove.yaml

# greenprint "Clean up"
# rm -rf auth.json rhel-9-y.repo
# unset ANSIBLE_CONFIG

greenprint "🎉 All tests passed."
exit 0
