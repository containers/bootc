prefix ?= /usr

all:
	cargo build --release
    
all-test:
	cargo build --release --all-features

install:
	install -D -t $(DESTDIR)$(prefix)/bin target/release/bootc

bin-archive: all
	$(MAKE) install DESTDIR=tmp-install && tar --zstd -C tmp-install -cf bootc.tar.zst . && rm tmp-install -rf

test-bin-archive: all-test
	$(MAKE) install DESTDIR=tmp-install && tar --zstd -C tmp-install -cf bootc.tar.zst . && rm tmp-install -rf

install-kola-tests:
	install -D -t $(DESTDIR)$(prefix)/lib/coreos-assembler/tests/kola/bootc tests/kolainst/*

vendor:
	cargo xtask $@
.PHONY: vendor

package-rpm:
	cargo xtask $@
.PHONY: package-rpm
