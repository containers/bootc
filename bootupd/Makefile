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

.PHONY: all
all:
	cargo build ${CARGO_ARGS}
	ln -f target/${PROFILE}/bootupd target/${PROFILE}/bootupctl

.PHONY: create-build-container
create-build-container:
	${CONTAINER_RUNTIME} build -t ${IMAGE_NAME} -f Dockerfile.build

.PHONY: build-in-container
build-in-container: create-build-container
	${CONTAINER_RUNTIME} run -ti --rm -v .:/srv/bootupd:z ${IMAGE_PREFIX}${IMAGE_NAME} make

.PHONY: install
install:
	mkdir -p "${DESTDIR}$(PREFIX)/bin" "${DESTDIR}$(LIBEXECDIR)"
	install -D -t "${DESTDIR}$(LIBEXECDIR)" target/${PROFILE}/bootupd
	ln -f ${DESTDIR}$(LIBEXECDIR)/bootupd ${DESTDIR}$(PREFIX)/bin/bootupctl

install-grub-static:
	install -m 644 -D -t ${DESTDIR}$(PREFIX)/lib/bootupd/grub2-static src/grub2/*.cfg
	install -m 755 -d ${DESTDIR}$(PREFIX)/lib/bootupd/grub2-static/configs.d

bin-archive:
	rm target/inst -rf
	$(MAKE) install install-grub-static DESTDIR=$$(pwd)/target/inst
	tar -C target/inst -c --zstd -f target/bootupd.tar.zst .
