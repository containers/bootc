DESTDIR ?=
PREFIX ?= /usr
RELEASE ?= 1

ifeq ($(RELEASE),1)
        PROFILE ?= release
        CARGO_ARGS = --release
else
        PROFILE ?= debug
        CARGO_ARGS =
endif

units = $(addprefix systemd/, bootupd.service)

.PHONY: all
all: $(units)
	cargo build ${CARGO_ARGS}

.PHONY: install-units
install-units: $(units)
	for unit in $(units); do install -D -m 644 --target-directory=$(DESTDIR)$(PREFIX)/lib/systemd/system/ $$unit; done

.PHONY: install
install: install-units
	install -D -t ${DESTDIR}$(PREFIX)/bin target/${PROFILE}/bootupd
