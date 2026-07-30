Name:           panini
Version:        0.1.0
Release:        1%{?dist}
Summary:        Press a Gleam (Erlang/BEAM) app into a single self-contained binary

License:        MIT
URL:            https://github.com/tsirysndr/panini
Packager:       Tsiry Sandratraina <tsiry.sndr@rocksky.app>

BuildArch:      x86_64

%description
panini bundles the BEAM runtime with your compiled Gleam app so it runs on a
machine with nothing installed. Supports OTP version selection and
cross-compilation. Statically linked (musl); no runtime dependencies.

%install
mkdir -p %{buildroot}/usr/local/bin
cp -r %{_sourcedir}/x86_64/usr %{buildroot}/

%files
/usr/local/bin/panini
