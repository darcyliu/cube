#!/bin/bash
# Download FreeBSD-14.4-RELEASE-amd64-disc1.iso. The ISO is gitignored.
set -euo pipefail
cd "$(dirname "$0")/.."
ISO="FreeBSD-14.4-RELEASE-amd64-disc1.iso"
BASE="https://download.freebsd.org/releases/amd64/amd64/ISO-IMAGES/14.4"
# This release publishes CHECKSUM.SHA256-<arch>, not CHECKSUM.SHA256.
SUM="CHECKSUM.SHA256-FreeBSD-14.4-RELEASE-amd64"
tmp="$(mktemp -d)"
partial="$ISO.partial"
cleanup() {
  rm -rf "$tmp"
  rm -f "$partial"
}
trap cleanup EXIT
echo "[VMM] Reading $SUM ..."
curl -fL --retry 3 --retry-delay 2 -o "$tmp/$SUM" "$BASE/$SUM"
want="$(awk -v iso="$ISO" '$1=="SHA256" && $2=="(" iso ")" { print $NF; exit }' "$tmp/$SUM")"
if [[ -z "${want}" ]]; then
  echo "$SUM has no SHA256 entry for $ISO" >&2
  exit 1
fi
echo "[VMM] SHA256 $want"
if [[ -f "$ISO" ]]; then
  got="$(shasum -a 256 "$ISO" | awk '{print $1}')"
  if [[ "$got" == "$want" ]]; then
    echo "[VMM] $ISO already present."
    trap - EXIT
    rm -rf "$tmp"
    exit 0
  fi
  echo "[VMM] $ISO SHA256 mismatch (got $got); re-downloading." >&2
fi
echo "[VMM] Downloading $ISO from $BASE/ ..."
curl -fL --retry 3 --retry-delay 2 -o "$partial" "$BASE/$ISO"
got="$(shasum -a 256 "$partial" | awk '{print $1}')"
if [[ "$got" != "$want" ]]; then
  echo "SHA256 mismatch for $ISO: got $got want $want" >&2
  exit 1
fi
mv "$partial" "$ISO"
trap - EXIT
rm -rf "$tmp"
echo "[VMM] Wrote $ISO ($(wc -c < "$ISO" | tr -d ' ') bytes)"
