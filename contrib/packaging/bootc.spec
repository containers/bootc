%bcond_without check
%if 0%{?rhel} >= 10 || 0%{?fedora} > 41
    %bcond_without ostree_ext
%else
    %bcond_with ostree_ext
%endif

%if 0%{?rhel}
    %bcond_without rhsm
%else
    %bcond_with rhsm
%endif

Name:           bootc
Version:        1.1.4
Release:        1%{?dist}
Summary:        Bootable container system

# Apache-2.0
# Apache-2.0 OR BSL-1.0
# Apache-2.0 OR MIT
# Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT
# BSD-3-Clause
# MIT
# MIT OR Apache-2.0
# Unlicense OR MIT
License:        Apache-2.0 AND BSD-3-Clause AND MIT AND (Apache-2.0 OR BSL-1.0) AND (Apache-2.0 OR MIT) AND (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT) AND (Unlicense OR MIT)
URL:            https://github.com/containers/bootc
Source0:        %{url}/releases/download/v%{version}/bootc-%{version}.tar.zstd
Source1:        %{url}/releases/download/v%{version}/bootc-%{version}-vendor.tar.zstd

# https://fedoraproject.org/wiki/Changes/EncourageI686LeafRemoval
ExcludeArch:    %{ix86}

BuildRequires: libzstd-devel
BuildRequires: make
BuildRequires: ostree-devel
BuildRequires: openssl-devel
%if 0%{?rhel}
BuildRequires: rust-toolset
%else
BuildRequires: cargo-rpm-macros >= 25
%endif
BuildRequires: systemd
# For tests
BuildRequires: skopeo ostree

# Backing storage tooling https://github.com/containers/composefs/issues/125
Requires: composefs
# For OS updates
Requires: ostree
Requires: skopeo
Requires: podman
# For bootloader updates
Recommends: bootupd

# A made up provides so that rpm-ostree can depend on it
%if %{with ostree_ext}
Provides: ostree-cli(ostree-container)
%endif

%description
%{summary}

%prep
%autosetup -p1 -a1
%cargo_prep -v vendor

%build
%if 0%{?fedora} || 0%{?rhel} >= 10
    %cargo_build %{?with_rhsm:-f rhsm}
%else
    %cargo_build %{?with_rhsm:--features rhsm}
%endif

%cargo_vendor_manifest
%cargo_license_summary
%{cargo_license} > LICENSE.dependencies

%install
%make_install INSTALL="install -p -c"
%if %{with ostree_ext}
make install-ostree-hooks DESTDIR=%{?buildroot}
%endif

%if %{with check}
%check
%cargo_test
%endif

%files
%license LICENSE-MIT
%license LICENSE-APACHE
%license LICENSE.dependencies
%license cargo-vendor.txt
%doc README.md
%{_bindir}/bootc
%{_prefix}/lib/bootc/
%{_prefix}/lib/systemd/system-generators/*
%if %{with ostree_ext}
%{_prefix}/libexec/libostree/ext/*
%endif
%{_unitdir}/*
%{_docdir}/bootc/*
%{_mandir}/man*/bootc*

%changelog
%autochangelog
