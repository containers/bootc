srpm:
	dnf -y install dnf-utils
	dnf -y builddep bootc
	# similar to https://github.com/actions/checkout/issues/760, but for COPR
	git config --global --add safe.directory '*'
	cargo install cargo-vendor-filterer
	cargo xtask package-srpm
	mv target/*.src.rpm $$outdir
