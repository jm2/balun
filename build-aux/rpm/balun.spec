Name:           balun
Version:        0.1.0
Release:        1%{?dist}
Summary:        A lightweight cross-platform HDHomeRun live TV viewer

License:        GPL-3.0-or-later
URL:            https://github.com/jm2/balun
Source0:        https://github.com/jm2/balun/archive/refs/tags/v%{version}.tar.gz

BuildRequires:  rust
BuildRequires:  cargo
BuildRequires:  bash
BuildRequires:  binutils
BuildRequires:  gcc
BuildRequires:  pkgconf-pkg-config
# The compliance validator run in %install and %check needs perl with Encode.
BuildRequires:  perl-interpreter
BuildRequires:  perl(Encode)
BuildRequires:  libadwaita-devel
BuildRequires:  gtk4-devel
BuildRequires:  gstreamer1-devel
BuildRequires:  pkgconfig(gtk4) >= 4.16
BuildRequires:  pkgconfig(libadwaita-1) >= 1.6
BuildRequires:  pkgconfig(gstreamer-1.0) >= 1.20
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

Requires:       gtk4 >= 4.16
Requires:       libadwaita >= 1.6
Requires:       gstreamer1 >= 1.20
Requires:       gstreamer1-plugins-base
Requires:       gstreamer1-plugins-good
Requires:       gstreamer1-plugins-bad-free
Requires:       gstreamer1-plugin-gtk4
Requires:       gstreamer1-plugin-libav

%description
Balun is a lightweight cross-platform HDHomeRun live TV viewer built with
GTK 4, libadwaita, and GStreamer. It discovers tuners on the local network
or behind a routed tunnel, lists their channel lineups, and plays live
television.

%prep
%autosetup -p1 -n %{name}-%{version}

%build
cargo build --release --locked --features desktop --bin balun

%install
# Install binary
install -D -p -m 0755 target/release/balun %{buildroot}%{_bindir}/balun

# Install desktop file
install -D -p -m 0644 data/io.github.jm2.Balun.desktop %{buildroot}%{_datadir}/applications/io.github.jm2.Balun.desktop

# Install metainfo
install -D -p -m 0644 data/io.github.jm2.Balun.metainfo.xml %{buildroot}%{_metainfodir}/io.github.jm2.Balun.metainfo.xml

# Install icons
for size in 16x16 24x24 32x32 48x48 64x64 128x128 256x256 512x512; do
    install -D -p -m 0644 data/icons/hicolor/${size}/apps/io.github.jm2.Balun.png \
        %{buildroot}%{_datadir}/icons/hicolor/${size}/apps/io.github.jm2.Balun.png
done
install -D -p -m 0644 data/icons/hicolor/scalable/apps/io.github.jm2.Balun.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/io.github.jm2.Balun.svg
install -D -p -m 0644 data/icons/hicolor/symbolic/apps/io.github.jm2.Balun-symbolic.svg \
    %{buildroot}%{_datadir}/icons/hicolor/symbolic/apps/io.github.jm2.Balun-symbolic.svg

# Validate the installed tree even when a build service skips its test phase.
build-aux/linux/validate-package-compliance.sh --tree "%{buildroot}"

%check
build-aux/linux/test-package-compliance.sh
build-aux/linux/validate-package-compliance.sh --elf target/release/balun
desktop-file-validate %{buildroot}%{_datadir}/applications/*.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/*.metainfo.xml

%files
%license LICENSE
%doc README.md
%{_bindir}/balun
%{_datadir}/applications/io.github.jm2.Balun.desktop
%{_metainfodir}/io.github.jm2.Balun.metainfo.xml
%{_datadir}/icons/hicolor/*/apps/io.github.jm2.Balun.png
%{_datadir}/icons/hicolor/scalable/apps/io.github.jm2.Balun.svg
%{_datadir}/icons/hicolor/symbolic/apps/io.github.jm2.Balun-symbolic.svg

%changelog
* Sat Sep 05 2026 John-Michael Mulesa <jmulesa@gmail.com> - 0.1.0-1
- Initial Fedora package.
