# Prebuilt upstream payload: nothing is compiled here, so debuginfo has no
# sources to point at and stripping would only churn the binary and the 23 MB
# bundled ONNX Runtime.
%global debug_package %{nil}
%global __strip /bin/true

# Both the bundled ONNX Runtime (SONAME libonnxruntime.so.1) and the LADSPA
# plugin are private: each is dlopen'd by absolute path and neither is meant to
# satisfy anyone else's dependency. Without this, RPM would advertise this
# package as providing the system libonnxruntime.
%global __provides_exclude_from ^/usr/lib/(hushmic|ladspa)/.*$
%global __requires_exclude ^libonnxruntime\\.so.*$

Name:           hushmic
Version:        0.2.1
Release:        1%{?dist}
Summary:        Real-time microphone noise suppression as a virtual mic

License:        MIT OR Apache-2.0
URL:            https://github.com/Fovty/hushmic
Source0:        %{url}/releases/download/v%{version}/hushmic-%{version}-x86_64.tar.gz

# The release ships a prebuilt x86-64 binary (glibc 2.35 floor).
ExclusiveArch:  x86_64

BuildRequires:  coreutils
BuildRequires:  findutils
BuildRequires:  desktop-file-utils

# pipewire-utils owns every pw-* tool the tray shells out to (pw-dump,
# pw-metadata, pw-record, pw-play). They are subprocesses, so RPM's ELF-based
# dependency generator cannot see them, and `pipewire` does NOT pull the utils
# in — without this line the package installs cleanly and then reports an empty
# microphone list. Fedora's analogue of the .deb's pipewire-bin dependency.
Requires:       pipewire
Requires:       pipewire-utils
Requires:       pipewire-pulseaudio
Requires:       wireplumber
Requires:       hicolor-icon-theme

%description
HushMic runs DPDFNet noise suppression on your microphone in real time and
exposes the cleaned audio as a PipeWire virtual microphone, so any application
can select it. Processing is local and CPU-only; audio never leaves the machine.

This package installs the binary from the upstream release, together with the
ONNX Runtime it was built and tested against.

%prep
%autosetup -n hushmic-%{version}-x86_64

%install
install -Dm755 bin/hushmic %{buildroot}%{_bindir}/hushmic

# NOTE: /usr/lib, deliberately NOT %%{_libdir} (which is /usr/lib64 here).
# The tray resolves the plugin, models and runtime relative to its own install
# prefix (/usr/bin/hushmic -> /usr) and only ever looks under <prefix>/lib —
# see Paths::resolve() in controller.rs. Installing into /usr/lib64 would leave
# those lookups falling back to a /usr/lib path that does not exist, and the
# filter chain would fail at enable() time.
install -Dm644 lib/ladspa/libdpdfnet_ladspa.so \
  %{buildroot}/usr/lib/ladspa/libdpdfnet_ladspa.so

# cp -a, not install: libonnxruntime.so -> .so.1 -> .so.1.27.0 is a soname
# symlink chain and only the real file may land as a regular file. Keep the
# unversioned .so even though rpmlint calls it a devel file: it is the exact
# path Paths::resolve() dlopens at runtime, not a link-time artifact.
install -d -m755 %{buildroot}/usr/lib/hushmic
cp -a lib/hushmic/libonnxruntime.so* %{buildroot}/usr/lib/hushmic/

install -Dm644 share/hushmic/models/dpdfnet8_48khz_hr.onnx \
  %{buildroot}%{_datadir}/hushmic/models/dpdfnet8_48khz_hr.onnx
install -Dm644 share/hushmic/models/dpdfnet2_48khz_hr.onnx \
  %{buildroot}%{_datadir}/hushmic/models/dpdfnet2_48khz_hr.onnx

install -Dm644 share/applications/hushmic.desktop \
  %{buildroot}%{_datadir}/applications/hushmic.desktop

# App icon + the tray status ladder (three SNI names x eight sizes); copying the
# tree keeps every size/state the release ships.
find share/icons -type f -name '*.png' -print0 | while IFS= read -r -d '' _icon; do
  install -Dm644 "$_icon" "%{buildroot}%{_prefix}/$_icon"
done

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/hushmic.desktop

%files
%license LICENSE-MIT LICENSE-APACHE
%{_bindir}/hushmic
%dir /usr/lib/ladspa
/usr/lib/ladspa/libdpdfnet_ladspa.so
%dir /usr/lib/hushmic
/usr/lib/hushmic/libonnxruntime.so*
%dir %{_datadir}/hushmic
%{_datadir}/hushmic/models/
%{_datadir}/applications/hushmic.desktop
%{_datadir}/icons/hicolor/*/*/hushmic*.png

%changelog
* Wed Jul 15 2026 Fovty <38868829+Fovty@users.noreply.github.com> - 0.2.1-1
- Initial package, built from the upstream v0.2.1 release tarball.
