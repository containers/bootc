%bcond_without check
%global __cargo_skip_build 0

%global crate bootupd

Name:           bootupd
Version:        0.2.9
Release:        1%{?dist}
Summary:        Bootloader updater

License:        ASL 2.0
URL:            https://crates.io/crates/bootupd
Source0:        https://github.com/coreos/bootupd/releases/download/v%{version}/bootupd-%{version}.tar.zstd
Source1:        https://github.com/coreos/bootupd/releases/download/v%{version}/bootupd-%{version}-vendor.tar.zstd

# For now, see upstream
# See https://github.com/coreos/fedora-coreos-tracker/issues/1716
%if 0%{?fedora} || 0%{?rhel} >= 10
ExcludeArch:   %{ix86}
%endif
BuildRequires: make
BuildRequires: cargo
# For autosetup -Sgit
BuildRequires: git
BuildRequires: openssl-devel
BuildRequires: systemd-devel

%description 
%{summary}

%files
%license LICENSE
%doc README.md
%{_bindir}/bootupctl
%{_libexecdir}/bootupd
%{_prefix}/lib/bootupd/grub2-static/

%prep
%autosetup -n %{crate}-%{version} -p1 -Sgit
tar -xv -f %{SOURCE1}
mkdir -p .cargo
cat >.cargo/config << EOF
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

%build
cargo build --release

%install
%make_install INSTALL="install -p -c"
make install-grub-static DESTDIR=%{?buildroot} INSTALL="%{__install} -p"

%changelog
* Tue Oct 18 2022 Colin Walters <walters@verbum.org> - 0.2.8-3
- Dummy changelog