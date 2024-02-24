## Integration Test

### Scenarios

Integration test includes two scenarios, `RPM build` and `bootc install/upgrade`.

1. RPM build scenario will build RPM for RHEL 9, CentOS Stream 9, and Fedora with mock.

2. bootc install/upgrade scenario will install and upgrade bootc image and have some system checking, such as check mount point/permission, run podman with root and rootless, check persistent log.

#### Run RPM Build Test

```shell
    podman run --rm --privileged -v ./:/workdir:z -e TEST_OS=$TEST_OS -e ARCH=$ARCH -e RHEL_REGISTRY_URL=$RHEL_REGISTRY_URL -e DOWNLOAD_NODE=$DOWNLOAD_NODE --workdir /workdir quay.io/fedora/fedora:40 ./tests/integration/mockbuild.sh
```

#### Run Integartion Test

Run on a shared test infrastructure using the [`testing farm`](https://docs.testing-farm.io/Testing%20Farm/0.1/cli.html) tool. For example, running on AWS.

Run `testing-farm` CLI from `quay.io/testing-farm/cli` container. Don't forget export the `TESTING_FARM_API_TOKEN` in your environment. To run RHEL test, `Red Hat Ranch` has to be used.

```shell
    export TESTING_FARM_API_TOKEN=<your-token>
    testing-farm request \
        --plan "aws" \
        --environment PLATFORM=$PLATFORM \
        --environment ARCH=$ARCH \
        --environment TEST_OS=$TEST_OS \
        --environment AWS_REGION=us-east-1 \
        --secret DOWNLOAD_NODE=$DOWNLOAD_NODE \
        --secret RHEL_REGISTRY_URL=$RHEL_REGISTRY_URL \
        --secret CERT_URL=$CERT_URL \
        --secret QUAY_USERNAME=$QUAY_USERNAME \
        --secret QUAY_PASSWORD=$QUAY_PASSWORD \
        --secret QUAY_SECRET=$QUAY_SECRET \
        --secret AWS_ACCESS_KEY_ID=$AWS_ACCESS_KEY_ID \
        --secret AWS_SECRET_ACCESS_KEY=$AWS_SECRET_ACCESS_KEY \
        --git-url <PR URL> \
        --git-ref <PR branch> \
        --compose "CentOS-Stream-9" \
        --arch $ARCH \
        --context "arch=$ARCH" \
        --timeout "120"
```

* AWS test needs environment variables `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and `AWS_REGION=us-east-1` have to be configured.

### Required environment variables

    TEST_OS        The OS to run the tests in. Currently supported values:
                       "rhel-9-4"
                       "centos-stream-9"
    ARCH           Test architecture
                       "x86_64"
                       "aarch64"

    PLATFORM       Run test on:
                       "aws"
    QUAY_USERNAME      quay.io username
    QUAY_PASSWORD      quay.io password
    QUAY_SECRET        Save into /etc/ostree/auth.json for authenticated registry
    DOWNLOAD_NODE      RHEL nightly compose download URL
    RHEL_REGISTRY_URL  RHEL bootc image URL
    CERT_URL           CA certificate download URL
    AWS_ACCESS_KEY_ID           AWS access key id
    AWS_SECRET_ACCESS_KEY       AWS secrety key
    AWS_REGION                  AWS region
                                    "us-east-1" RHEL AWS EC2 image is only available in this region
    TESTING_FARM_API_TOKEN      Required by Testing Farm API
