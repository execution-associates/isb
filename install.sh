#!/bin/sh
# Install isb from a GitHub release, checking the release signature first.
#
#   curl -fsSL https://github.com/execution-associates/isb/releases/latest/download/install.sh | sh
#   sh install.sh [VERSION]      # e.g. 1.2.0; default the latest release
#
# ISB_INSTALL_DIR  where isb goes (default ~/.local/bin; /usr/local/bin as root)
# ISB_OPENSSL      the openssl to check with (OpenSSL 3 or later: LibreSSL,
#                  macOS's own openssl, cannot check Ed25519 signatures)
#
# The release's SHA256SUMS must carry a valid Ed25519 signature
# (SHA256SUMS.sig) by the isb release key below, the key `isb update` checks
# too, and the tarball must match SHA256SUMS. Otherwise nothing is installed.
# The installed binary then keeps itself current with `isb update`.
set -eu

REPO=execution-associates/isb
# One of RELEASE_KEYS in crates/isb-core/src/self_update.rs (a test keeps
# them in step). PEM is the same key; it is checked against KEY before use.
KEY=4f08d05a2ffaf58f40d4d0e658a9934e5246de1b472ccd2143af6c928adfd51a
PEM='-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEATwjQWi/69Y9A1NDmWKmTTlJG3htHLM0hQ69skorf1Ro=
-----END PUBLIC KEY-----'

say() { printf 'isb install: %s\n' "$*" >&2; }
die() {
	say "$*"
	exit 1
}

command -v curl >/dev/null 2>&1 || die "needs curl"
command -v tar >/dev/null 2>&1 || die "needs tar"

ssl=
for c in ${ISB_OPENSSL:-} openssl /opt/homebrew/opt/openssl@3/bin/openssl /usr/local/opt/openssl@3/bin/openssl; do
	if "$c" version 2>/dev/null | grep -Eq '^OpenSSL ([3-9]|[1-9][0-9])\.'; then
		ssl=$c
		break
	fi
done
[ -n "$ssl" ] || die "needs OpenSSL 3 or later to check the release signature \
(macOS: brew install openssl@3; Debian/Ubuntu: apt install openssl)"

case "$(uname -s)/$(uname -m)" in
Linux/x86_64 | Linux/amd64) target=x86_64-unknown-linux-musl ;;
Linux/aarch64 | Linux/arm64) target=aarch64-unknown-linux-musl ;;
Darwin/arm64 | Darwin/aarch64) target=aarch64-apple-darwin ;;
Darwin/x86_64) target=x86_64-apple-darwin ;;
*) die "no isb release is built for $(uname -s)/$(uname -m); build from source with \`cargo install isb\`" ;;
esac

version=${1:-${ISB_VERSION:-}}
if [ -z "$version" ]; then
	# The latest release, from where /releases/latest redirects (no API rate limit).
	url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") ||
		die "cannot reach github.com to find the latest release"
	version=${url##*/}
fi
version=${version#v}
case "$version" in
[0-9]*.[0-9]*.[0-9]*) ;;
*) die "not a release version: $version" ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' INT TERM
base=https://github.com/$REPO/releases/download/v$version
fetch() { curl -fsSL --retry 3 -o "$tmp/$1" "$base/$1"; }

fetch SHA256SUMS || die "cannot download $base/SHA256SUMS (no release v$version?)"
fetch SHA256SUMS.sig || die "release v$version is not signed; only 1.1.1 and later can be installed"

printf '%s\n' "$PEM" >"$tmp/key.pem"
pem_key=$("$ssl" pkey -pubin -in "$tmp/key.pem" -outform DER | tail -c 32 | od -An -tx1 | tr -d ' \n')
[ "$pem_key" = "$KEY" ] || die "this installer's PEM is not its KEY; refusing to trust it"
"$ssl" pkeyutl -verify -rawin -pubin -inkey "$tmp/key.pem" \
	-in "$tmp/SHA256SUMS" -sigfile "$tmp/SHA256SUMS.sig" >/dev/null 2>&1 ||
	die "the signature on v$version's SHA256SUMS does not verify; refusing to install"

asset=isb-v$version-$target.tar.gz
want=$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
[ -n "$want" ] || die "v$version has no $asset"
say "downloading isb $version ($target)"
fetch "$asset" || die "cannot download $base/$asset"
got=$("$ssl" dgst -sha256 -r "$tmp/$asset" | cut -d' ' -f1)
[ "$got" = "$want" ] || die "$asset does not match the signed SHA256SUMS; refusing to install"

tar -xzf "$tmp/$asset" -C "$tmp"
bin=$tmp/isb-v$version-$target/isb
[ "$("$bin" --version 2>/dev/null)" = "isb $version" ] || die "the downloaded isb does not run here"

if [ -n "${ISB_INSTALL_DIR:-}" ]; then
	dir=$ISB_INSTALL_DIR
elif [ "$(id -u)" = 0 ]; then
	dir=/usr/local/bin
else
	dir=$HOME/.local/bin
fi
mkdir -p "$dir"
# Copy next to the target, then rename: atomic, and safe while isb runs.
cp "$bin" "$dir/.isb.new.$$"
chmod 755 "$dir/.isb.new.$$"
mv -f "$dir/.isb.new.$$" "$dir/isb"
say "installed isb $version at $dir/isb"

case ":$PATH:" in
*":$dir:"*) ;;
*) say "$dir is not on your PATH; add it to run \`isb\`" ;;
esac
found=$(command -v isb 2>/dev/null || true)
if [ -n "$found" ] && [ "$found" != "$dir/isb" ]; then
	say "warning: \`isb\` on your PATH is $found, which comes before $dir/isb"
	case "$found" in
	*/mise/*) say "remove the mise install (mise does not check isb's release signature): mise unuse -g github:execution-associates/isb" ;;
	esac
fi
if [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/isb.service" ]; then
	say "isb serve runs as a user service: \`$dir/isb serve install\` points it at this binary"
fi
