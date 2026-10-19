#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bundle_dir="$ROOT/target/release/bundle"
arch="$(dpkg --print-architecture)"
case "$arch" in
  arm64|amd64) ;;
  *) printf 'Unsupported Linux architecture: %s\n' "$arch" >&2; exit 1 ;;
esac

shopt -s nullglob
packages=("$bundle_dir"/deb/slopcast_*_"$arch".deb)
if (( ${#packages[@]} != 1 )); then
  printf 'Expected one %s deb in %s/deb, found %s\n' "$arch" "$bundle_dir" "${#packages[@]}" >&2
  exit 1
fi
package="${packages[0]}"
test "$(dpkg-deb --field "$package" Architecture)" = "$arch"
version="$(dpkg-deb --field "$package" Version)"

# Archive the installed payload, not a .deb inside another archive.
payload_dir="$(mktemp -d /tmp/slopcast-tar.XXXXXX)"
trap 'rm -r -- "$payload_dir"' EXIT
dpkg-deb --extract "$package" "$payload_dir"
archive="$bundle_dir/Slopcast-$version-linux-$arch.tar.gz"
tar -czf "$archive" -C "$payload_dir" .
printf 'Created %s\n' "$archive"
