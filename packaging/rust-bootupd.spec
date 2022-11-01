%bcond_without check
%global __cargo_skip_build 0

%global crate bootupd

Name:           rust-%{crate}
Version:        0.1.0
Release:        3%{?dist}
Summary:        Bootloader updater

License:        ASL 2.0
URL:            https://crates.io/crates/bootupd
Source:         %{crates_source}

# For now, see upstream
ExclusiveArch:  x86_64
BuildRequires:  openssl-devel
%if 0%{?rhel} && !0%{?eln}
BuildRequires: rust-toolset
%else
BuildRequires: rust-packaging
%endif
BuildRequires:  systemd

%global _description %{expand:
Bootloader updater}
%description %{_description}

%package     -n %{crate}
Summary:        %{summary}
License:        ASL 2.0
%{?systemd_requires}

%description -n %{crate} %{_description}

%files -n %{crate}
%license LICENSE
%doc README.md
%{_bindir}/bootupctl
%{_libexecdir}/bootupd
%{_unitdir}/*

%prep
%autosetup -n %{crate}-%{version_no_tilde} -p1
%cargo_prep

%generate_buildrequires
%cargo_generate_buildrequires

%build
%cargo_build

%install
%make_install INSTALL="install -p -c"

%changelog
* Fri Sep 11 2020 Colin Walters <walters@verbum.org> - 0.1.0-3
- Initial package

