srpm:
	dnf -y install cargo git openssl-devel ostree-devel
	# similar to https://github.com/actions/checkout/issues/760, but for COPR
	git config --global --add safe.directory '*'
	cargo install cargo-vendor-filterer
	cargo xtask package-srpm
	mv target/*.src.rpm $$outdir
