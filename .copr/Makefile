srpm:
	dnf -y install 'dnf5-command(builddep)'
	dnf -y install dnf-utils git
	# similar to https://github.com/actions/checkout/issues/760, but for COPR
	git config --global --add safe.directory '*'
	dnf -y builddep ./contrib/packaging/bootc.spec
	cargo install cargo-vendor-filterer
	cargo xtask package-srpm
	mv target/*.src.rpm $$outdir
