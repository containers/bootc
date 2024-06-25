#!/bin/bash
set -exuo pipefail

ARCH=$(uname -m)

# Colorful output.
function greenprint {
  echo -e "\033[1;32m[$(date -Isecond)] ${1}\033[0m"
}
function redprint {
    echo -e "\033[1;31m[$(date -Isecond)] ${1}\033[0m"
}

greenprint "📥 Install required packages"
dnf install -y cargo zstd git libzstd-devel openssl-devel ostree-devel rpm-build mock podman skopeo jq
cargo install cargo-vendor-filterer

greenprint "⛏ Build archive"
cargo xtask package-srpm

greenprint "📋 Get target tmp folder path"
shopt -s extglob
TARGET_FOLDER=(target/.tmp*)

case "$TEST_OS" in
    "rhel-9"*)
        TEMPLATE="rhel-9.tpl"
        greenprint "📝 update mock rhel-9 template"
        # disable subscription for nightlies
        sed -i "s/config_opts\['redhat_subscription_required'\] = True/config_opts['redhat_subscription_required'] = False/" /etc/mock/templates/"$TEMPLATE"
        # delete default cdn compose and add nightly compose
        sed -i '/user_agent/q' /etc/mock/templates/"$TEMPLATE"
        if [[ "$TEST_OS" == "rhel-9-4" ]]; then
            BATCH_COMPOSE="updates/"
            LATEST_COMPOSE_ID="latest-RHEL-9.4.0"
        else
            BATCH_COMPOSE=""
            LATEST_COMPOSE_ID="latest-RHEL-9.5.0"
        fi
        tee -a /etc/mock/templates/"$TEMPLATE" > /dev/null << EOF
[BaseOS]
name=Red Hat Enterprise Linux - BaseOS
baseurl=http://${DOWNLOAD_NODE}/rhel-9/nightly/${BATCH_COMPOSE}RHEL-9/${LATEST_COMPOSE_ID}/compose/BaseOS/\$basearch/os/
enabled=1
gpgcheck=0

[AppStream]
name=Red Hat Enterprise Linux - AppStream
baseurl=http://${DOWNLOAD_NODE}/rhel-9/nightly/${BATCH_COMPOSE}RHEL-9/${LATEST_COMPOSE_ID}/compose/AppStream/\$basearch/os/
enabled=1
gpgcheck=0

[CRB]
name=Red Hat Enterprise Linux - CRB
baseurl=http://${DOWNLOAD_NODE}/rhel-9/nightly/${BATCH_COMPOSE}RHEL-9/${LATEST_COMPOSE_ID}/compose/CRB/\$basearch/os/
enabled=1
gpgcheck=0
"""
EOF
        MOCK_CONFIG="rhel-9-${ARCH}"
        ;;
    "centos-stream-9")
        MOCK_CONFIG="centos-stream-9-${ARCH}"
        ;;
    "fedora-40")
        MOCK_CONFIG="fedora-40-${ARCH}"
        ;;
    "fedora-41")
        MOCK_CONFIG="fedora-41-${ARCH}"
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
