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
ExclusiveArch: x86_64 aarch64
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
%{_unitdir}/*

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

%post        -n %{crate}
%systemd_post bootupd.service bootupd.socket

%preun       -n %{crate}
%systemd_preun bootupd.service bootupd.socket

%postun      -n %{crate}
%systemd_postun bootupd.service bootupd.socket

%changelog
* Tue Oct 18 2022 Colin Walters <walters@verbum.org> - 0.2.8-3
- Dummy changelog