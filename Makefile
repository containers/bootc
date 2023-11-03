prefix ?= /usr

all:
	cargo build --release
    
all-test:
	cargo build --release --all-features

install:
	install -D -m 0755 -t $(DESTDIR)$(prefix)/bin target/release/bootc
	install -d $(DESTDIR)$(prefix)/lib/bootc/install
	# Support installing pre-generated man pages shipped in source tarball, to avoid
	# a dependency on pandoc downstream
	if test -d man; then install -D -m 0644 -t $(DESTDIR)$(prefix)/share/man/man5 man/*.5; fi
	if test -d man; then install -D -m 0644 -t $(DESTDIR)$(prefix)/share/man/man8 man/*.8; fi

# These are not installed by default; one recommendation is to put them in a separate
# sub-package or sub-component.
install-systemd-auto:
	install -D -m 0644 -t $(DESTDIR)/$(prefix)/lib/systemd/system systemd/*.service systemd/*.timer

bin-archive: all
	$(MAKE) install DESTDIR=tmp-install && tar --zstd -C tmp-install -cf target/bootc.tar.zst . && rm tmp-install -rf

test-bin-archive: all-test
	$(MAKE) install DESTDIR=tmp-install && tar --zstd -C tmp-install -cf target/bootc.tar.zst . && rm tmp-install -rf

install-kola-tests:
	install -D -t $(DESTDIR)$(prefix)/lib/coreos-assembler/tests/kola/bootc tests/kolainst/*

validate:
	cargo fmt
	cargo clippy
.PHONY: validate

vendor:
	cargo xtask $@
.PHONY: vendor

package-rpm:
	cargo xtask $@
.PHONY: package-rpm
