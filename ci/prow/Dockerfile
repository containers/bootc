FROM registry.ci.openshift.org/coreos/cosa-buildroot:latest as builder
WORKDIR /src
COPY . .
RUN make && make install DESTDIR=/cosa/component-install
RUN make -C tests/kolainst install DESTDIR=/cosa/component-tests
# Uncomment this to fake a build to test the code below
# RUN mkdir -p /cosa/component-install/usr/bin && echo foo > /cosa/component-install/usr/bin/foo

FROM registry.ci.openshift.org/coreos/coreos-assembler:latest
WORKDIR /srv
USER root
# Install our built binaries as overrides for the target build
COPY --from=builder /cosa/component-install/ /srv/overrides/rootfs/
# Copy and install tests too
COPY --from=builder /cosa/component-tests /srv/tmp/component-tests
# Install tests
RUN rsync -rlv /srv/tmp/component-tests/ / && rm -rf /srv/tmp/component-tests
COPY --from=builder /src/ci/prow/fcos-e2e.sh /usr/bin/fcos-e2e
USER builder
