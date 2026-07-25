%global _misspelled_header_terminators 1

Name:           zing
Version:        0.1.0
Release:        1%{?dist}
Summary:        A modern HTTP downloader with segmented concurrent downloads

License:        GPL-3.0-only
URL:            https://github.com/TharukRenuja/rxdl
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo

Requires:       libc.so.6()(64bit)

%description
A modern HTTP downloader with support for HTTP/1.1, HTTP/2, and HTTP/3.
Features include segmented concurrent downloads, daemon mode, and adaptive
connection management.

%prep
%setup -q

%build
cargo build --release --workspace

%install
%define _bindir %{_prefix}/bin
install -Dm0755 target/release/zing %{buildroot}%{_bindir}/zing
install -Dm0755 target/release/zing-daemon %{buildroot}%{_bindir}/zing-daemon

# Man page
mkdir -p %{buildroot}%{_mandir}/man1
target/release/zing man > %{buildroot}%{_mandir}/man1/zing.1

# Shell completions
mkdir -p %{buildroot}%{_datadir}/bash-completion/completions
target/release/zing completions bash > %{buildroot}%{_datadir}/bash-completion/completions/zing

mkdir -p %{buildroot}%{_datadir}/zsh/site-functions
target/release/zing completions zsh > %{buildroot}%{_datadir}/zsh/site-functions/_zing

mkdir -p %{buildroot}%{_datadir}/fish/vendor_completions.d
target/release/zing completions fish > %{buildroot}%{_datadir}/fish/vendor_completions.d/zing.fish

%check
cargo test --workspace --release

%files
%{_bindir}/zing
%{_bindir}/zing-daemon
%{_mandir}/man1/zing.1*
%{_datadir}/bash-completion/completions/zing
%{_datadir}/zsh/site-functions/_zing
%{_datadir}/fish/vendor_completions.d/zing.fish

%doc README.md

%changelog
* Sat Jul 25 2026 Tharuk Renuja <tharuk@example.com> - 0.1.0-1
- Initial release
