#!/bin/bash
set -exuo pipefail

# Colorful output.
function greenprint {
  echo -e "\033[1;32m[$(date -Isecond)] ${1}\033[0m"
}
function redprint {
    echo -e "\033[1;31m[$(date -Isecond)] ${1}\033[0m"
}

greenprint "📥 Install required packages"
dnf install -y cargo zstd git openssl-devel ostree-devel rpm-build mock podman skopeo jq
cargo install cargo-vendor-filterer

greenprint "⛏ Build archive"
cargo xtask package-srpm

greenprint "📋 Get target tmp folder path"
shopt -s extglob
TARGET_FOLDER=(target/.tmp*)

case "$TEST_OS" in
    "rhel-9-4")
        TEMPLATE="rhel-9.tpl"
        greenprint "📝 update mock rhel-9 template"
        # disable subscription for nightlies
        sed -i "s/config_opts\['redhat_subscription_required'\] = True/config_opts['redhat_subscription_required'] = False/" /etc/mock/templates/"$TEMPLATE"
        # delete default cdn compose and add nightly compose
        IMAGE_NAME="rhel9-rhel_bootc"
        TIER1_IMAGE_URL="${RHEL_REGISTRY_URL}/${IMAGE_NAME}:rhel-9.4"
        CURRENT_COMPOSE_RHEL94=$(skopeo inspect --tls-verify=false "docker://${TIER1_IMAGE_URL}" | jq -r '.Labels."redhat.compose-id"')
        sed -i '/user_agent/q' /etc/mock/templates/"$TEMPLATE"
        tee -a /etc/mock/templates/"$TEMPLATE" > /dev/null << EOF
[BaseOS]
name=Red Hat Enterprise Linux - BaseOS
baseurl=http://${DOWNLOAD_NODE}/rhel-9/composes/RHEL-9/${CURRENT_COMPOSE_RHEL94}/compose/BaseOS/\$basearch/os/
enabled=1
gpgcheck=0

[AppStream]
name=Red Hat Enterprise Linux - AppStream
baseurl=http://${DOWNLOAD_NODE}/rhel-9/composes/RHEL-9/${CURRENT_COMPOSE_RHEL94}/compose/AppStream/\$basearch/os/
enabled=1
gpgcheck=0

[CRB]
name = Red Hat Enterprise Linux - CRB
baseurl = http://${DOWNLOAD_NODE}/rhel-9/composes/RHEL-9/${CURRENT_COMPOSE_RHEL94}/compose/CRB/\$basearch/os/
enabled = 1
gpgcheck = 0
"""
EOF
        MOCK_CONFIG="rhel-9-${ARCH}"
        ;;
    "centos-stream-9")
        MOCK_CONFIG="centos-stream-9-${ARCH}"
        ;;
    *)
        redprint "Variable TEST_OS has to be defined"
        exit 1
        ;;
esac

greenprint "🧬 Using mock config: ${MOCK_CONFIG}"

greenprint "✏ Adding user to mock group"
usermod -a -G mock "$(whoami)"

greenprint "🎁 Building SRPM"
mock -r "$MOCK_CONFIG" --buildsrpm \
  --spec "${TARGET_FOLDER[0]}/bootc.spec" \
  --config-opts=cleanup_on_failure=False \
  --config-opts=cleanup_on_success=True \
  --sources "${TARGET_FOLDER[0]}" \
  --resultdir ./tests/integration/build

greenprint "🎁 Building RPMs"
mock -r "$MOCK_CONFIG" \
    --config-opts=cleanup_on_failure=False \
    --config-opts=cleanup_on_success=True \
    --resultdir "./tests/integration/build" \
    ./tests/integration/build/*.src.rpm
