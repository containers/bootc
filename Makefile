DESTDIR ?=
PREFIX ?= /usr
LIBEXECDIR ?= ${PREFIX}/libexec
RELEASE ?= 1
CONTAINER_RUNTIME ?= podman
IMAGE_PREFIX ?=
IMAGE_NAME ?= bootupd-build

ifeq ($(RELEASE),1)
        PROFILE ?= release
        CARGO_ARGS = --release
else
        PROFILE ?= debug
        CARGO_ARGS =
endif

ifeq ($(CONTAINER_RUNTIME), podman)
        IMAGE_PREFIX = localhost/
endif

units = $(addprefix systemd/, bootupd.service bootupd.socket)

.PHONY: all
all: $(units)
	cargo build ${CARGO_ARGS}
	ln -f target/${PROFILE}/bootupd target/${PROFILE}/bootupctl

.PHONY: create-build-container
create-build-container:
	${CONTAINER_RUNTIME} build -t ${IMAGE_NAME} -f Dockerfile.build

.PHONY: build-in-container
build-in-container: create-build-container
	${CONTAINER_RUNTIME} run -ti --rm -v .:/srv/bootupd:z ${IMAGE_PREFIX}${IMAGE_NAME} make

.PHONY: install-units
install-units: $(units)
	for unit in $(units); do install -D -m 644 --target-directory=$(DESTDIR)$(PREFIX)/lib/systemd/system/ $$unit; done

.PHONY: install
install: install-units
	mkdir -p "${DESTDIR}$(PREFIX)/bin" "${DESTDIR}$(LIBEXECDIR)"
	install -D -t "${DESTDIR}$(LIBEXECDIR)" target/${PROFILE}/bootupd
	ln -f ${DESTDIR}$(LIBEXECDIR)/bootupd ${DESTDIR}$(PREFIX)/bin/bootupctl
	install -d "${DESTDIR}$(PREFIX)/lib/systemd/system/multi-user.target.wants"
	ln -s ../bootupd.socket "${DESTDIR}$(PREFIX)/lib/systemd/system/multi-user.target.wants"
