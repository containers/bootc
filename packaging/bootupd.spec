%bcond_without check
%global __cargo_skip_build 0

%global crate bootupd

Name:           %{crate}
Version:        0.1.0
Release:        1%{?dist}
Summary:        Bootloader updater

License:        ASL 2.0
URL:            https://crates.io/crates/bootupd
Source0:        https://crates.io/api/v1/crates/%{crate}/%{version}/download#/%{crate}-%{version}.crate
Source1:        https://github.com/coreos/bootupd/releases/download/v%{version}/%{crate}-%{version}-vendor.tar.gz

# For now, see upstream
ExclusiveArch:  x86_64
BuildRequires:  openssl-devel
%if 0%{?rhel} && !0%{?eln}
BuildRequires: rust-toolset
%else
BuildRequires: cargo
BuildRequires: rust
%endif
BuildRequires:  systemd

%description
%{summary}

%files
%license LICENSE
%doc README.md
%{_libexecdir}/bootupd
%{_unitdir}/*

%prep
# FIXME shouldn't both source0/source be extracted with this?
%autosetup -n %{crate}-%{version} -p1
%autosetup -n %{crate}-%{version} -p1 -a 1
# https://github.com/rust-lang-nursery/error-chain/pull/289
find -name '*.rs' -executable -exec chmod a-x {} \;
mkdir -p .cargo
cat >.cargo/config << 'EOF'
[source.crates-io]
registry = 'https://github.com/rust-lang/crates.io-index'
replace-with = 'vendored-sources'

[source.vendored-sources]
directory = './vendor'
EOF

%build
%cargo_build

%install
%make_install INSTALL="install -p -c"
