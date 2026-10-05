#!/bin/sh
# Put the signed release's isb for this machine in DIR, for the SDK packages
# to bundle: the exact bytes the release signed, not a second build.
#
#   scripts/release-binary.sh VERSION DIR
#
# The SDK workflows start on the same tag push as release.yml, so this waits
# (up to an hour) for the release's assets, then installs with install.sh,
# which refuses anything whose signature or checksum does not verify.
set -eu

[ $# -eq 2 ] || { echo "usage: $0 VERSION DIR" >&2; exit 2; }
version=${1#v}
dir=$2
here=$(cd "$(dirname "$0")/.." && pwd)
base=https://github.com/execution-associates/isb/releases/download/v$version

i=0
until curl -fsIL -o /dev/null "$base/SHA256SUMS.sig" && curl -fsIL -o /dev/null "$base/SHA256SUMS"; do
	i=$((i + 1))
	[ "$i" -le 120 ] || { echo "release v$version has no signed SHA256SUMS after an hour" >&2; exit 1; }
	echo "waiting for release v$version ($i)"
	sleep 30
done

ISB_INSTALL_DIR=$dir sh "$here/install.sh" "$version"
