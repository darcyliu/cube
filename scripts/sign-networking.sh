#!/usr/bin/env bash
# Sign vmm with com.apple.vm.networking using a Developer identity + provisioning profile.
# Ad-hoc signing cannot use this entitlement (AMFI will SIGKILL the process).
#
# Prerequisites (one-time, Apple Developer Program):
#   1. Register a Mac App ID (e.g. com.example.vmm)
#   2. Request/enable VM Networking (com.apple.vm.networking) on that App ID
#   3. Create a Mac Development (or Developer ID) provisioning profile for it
#   4. Install the signing certificate in your keychain (Xcode account sync is fine)
#
# Usage:
#   IDENTITY="Apple Development: Your Name (TEAMID)" \
#   PROFILE="$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles/….provisionprofile" \
#   BUNDLE_ID="com.example.vmm" \
#   make sign-networking
#
# Produces: dist/cube.app  (run: open dist/cube.app  or  dist/cube.app/Contents/MacOS/cube …)
# Also copies a convenience binary link note in dist/README-networking-sign.txt

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

CUBE_BIN="${CUBE_BIN:-${VMM_BIN:-target/debug/cube}}"
APP_DIR="${APP_DIR:-dist/cube.app}"
BUNDLE_ID="${BUNDLE_ID:-net.neuralengine.cube}"
TEAM_ID="${TEAM_ID:-ZV8TTD4Z45}"
IDENTITY="${IDENTITY:-}"
PROFILE="${PROFILE:-}"
ENTITLEMENTS="${ENTITLEMENTS:-entitlements.networking.plist}"

die() { echo "[sign-networking] ERROR: $*" >&2; exit 1; }

if [[ ! -f "$CUBE_BIN" ]]; then
  die "missing $CUBE_BIN — run: make build"
fi
if [[ ! -f "$ENTITLEMENTS" ]]; then
  die "missing $ENTITLEMENTS"
fi
if [[ -z "$IDENTITY" ]]; then
  echo "[sign-networking] Available signing identities:"
  security find-identity -v -p codesigning || true
  die "set IDENTITY=\"Apple Development: Your Name (TEAMID)\""
fi
if [[ -z "$PROFILE" || ! -f "$PROFILE" ]]; then
  die "set PROFILE=/path/to/YourProfile.provisionprofile (file must exist)"
fi

# Decode profile and sanity-check bundle id + entitlement if possible
TMP_PROF="$(mktemp)"
cleanup() { rm -f "$TMP_PROF"; }
trap cleanup EXIT
if security cms -D -i "$PROFILE" >"$TMP_PROF" 2>/dev/null; then
  PROF_APPID="$(/usr/libexec/PlistBuddy -c 'Print :Entitlements:application-identifier' "$TMP_PROF" 2>/dev/null || true)"
  # application-identifier is TEAMID.bundleid — strip team prefix
  if [[ -n "$PROF_APPID" ]]; then
    PROF_BUNDLE="${PROF_APPID#*.}"
    if [[ "$PROF_BUNDLE" != "$BUNDLE_ID" ]]; then
      echo "[sign-networking] WARNING: BUNDLE_ID=$BUNDLE_ID but profile app id is $PROF_APPID"
      echo "[sign-networking]          Continuing; set BUNDLE_ID to match the profile if codesign fails."
    fi
  fi
  if ! grep -q 'com.apple.vm.networking' "$TMP_PROF"; then
    echo "[sign-networking] WARNING: profile XML does not mention com.apple.vm.networking."
    echo "[sign-networking]          Request/enable VM Networking on the App ID, then regenerate the profile."
  fi
else
  echo "[sign-networking] WARNING: could not decode provisioning profile (security cms -D failed)."
fi

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>cube</string>
	<key>CFBundleIdentifier</key>
	<string>${BUNDLE_ID}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>cube</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>0.1.0</string>
	<key>CFBundleVersion</key>
	<string>1</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0</string>
</dict>
</plist>
PLIST

cp "$CUBE_BIN" "$APP_DIR/Contents/MacOS/cube"
chmod +x "$APP_DIR/Contents/MacOS/cube"
cp "$PROFILE" "$APP_DIR/Contents/embedded.provisionprofile"

echo "[sign-networking] Signing $APP_DIR with identity: $IDENTITY"
codesign --force --deep --sign "$IDENTITY" \
  --entitlements "$ENTITLEMENTS" \
  --identifier "$BUNDLE_ID" \
  "$APP_DIR"

echo "[sign-networking] Verifying…"
codesign --verify --verbose=2 "$APP_DIR"
echo "[sign-networking] Entitlements on binary:"
codesign -d --entitlements - --xml "$APP_DIR/Contents/MacOS/cube" 2>/dev/null | plutil -p - || true

mkdir -p dist
cat > dist/README-networking-sign.txt <<TXT
Signed app: $APP_DIR
Bundle ID:  $BUNDLE_ID
Identity:   $IDENTITY

Run (no sudo needed for vmnet *if* the profile grants com.apple.vm.networking):
  $APP_DIR/Contents/MacOS/cube --uefi --disk freebsd-installed.img --net vmnet

or:
  $APP_DIR/Contents/MacOS/cube … --net bridge:en0

Host PF NAT may still need sudo once for shared/NAT mode internet; bridge mode skips PF NAT.

Ad-hoc make sign remains the default for local sudo+vmnet workflows.
TXT

echo "[sign-networking] Done → $APP_DIR"
echo "[sign-networking] See dist/README-networking-sign.txt"
