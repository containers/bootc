FROM quay.io/coreos-assembler/fcos-buildroot:testing-devel as builder
WORKDIR /src
COPY . .
RUN make && make install DESTDIR=/cosa/component-install
RUN make -C tests/kolainst install DESTDIR=/cosa/component-tests
# Uncomment this to fake a build to test the code below
# RUN mkdir -p /cosa/component-install/usr/bin && echo foo > /cosa/component-install/usr/bin/foo

FROM quay.io/coreos-assembler/coreos-assembler:latest
WORKDIR /srv
# Install our built binaries as overrides for the target build
COPY --from=builder /cosa/component-install/ /srv/overrides/rootfs/
# Copy and install tests too
COPY --from=builder /cosa/component-tests /srv/tmp/component-tests
# And fix permissions
RUN sudo chown -R builder: /srv/*
# Install tests
USER root
RUN rsync -rlv /srv/tmp/component-tests/ / && rm -rf /srv/tmp/component-tests
USER builder
COPY --from=builder /src/ci/prow/fcos-e2e.sh /usr/bin/fcos-e2e
