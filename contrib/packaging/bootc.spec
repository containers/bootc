%bcond_without check

Name:           bootc
Version:        0.1
Release:        1%{?dist}
Summary:        Boot containers

License:        ASL 2.0
URL:            https://github.com/containers/bootc
Source0:        https://github.com/containers/bootc/releases/download/v%{version}/bootc-%{version}.tar.zstd
Source1:        https://github.com/containers/bootc/releases/download/v%{version}/bootc-%{version}-vendor.tar.zstd

BuildRequires: make
BuildRequires: openssl-devel
BuildRequires: cargo
BuildRequires: systemd
# For autosetup -Sgit
BuildRequires: git
BuildRequires: zlib-devel
BuildRequires: ostree-devel
BuildRequires: openssl-devel
BuildRequires: systemd-devel
BuildRequires: libzstd-devel

%description
%{summary}

%files
%license LICENSE-APACHE LICENSE-MIT
%doc README.md
%{_bindir}/bootc
%{_prefix}/lib/systemd/system-generators/*
%{_prefix}/lib/bootc
%{_unitdir}/*
%{_docdir}/bootc/*
%{_mandir}/man*/bootc*

%prep
%autosetup -p1 -Sgit
tar -xv -f %{SOURCE1}
mkdir -p .cargo
cat >>.cargo/config.toml << EOF
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

%build
make

%install
%make_install INSTALL="install -p -c"

%changelog
%autochangelog
