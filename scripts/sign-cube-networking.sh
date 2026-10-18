#!/usr/bin/env bash
# Sign Cube.app and embedded cube with com.apple.vm.networking using an Apple Developer identity + provisioning profile.
#
# Prerequisites (Apple Developer Program):
#   1. Mac App ID registered (e.g. net.neuralengine.cube)
#   2. "VM Networking" (com.apple.vm.networking) enabled on that App ID
#   3. Mac Development / Developer ID provisioning profile downloaded
#
# Usage:
#   IDENTITY="Apple Development: Your Name (TEAMID)" \
#   PROFILE="Cube_Dev.provisionprofile" \
#   BUNDLE_ID="net.neuralengine.cube" \
#   make sign-cube-networking
#
# Or simply:
#   make sign-cube-networking   (will auto-detect identity and local profile)

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_DIR="${APP_DIR:-dist/Cube.app}"
BUNDLE_ID="${BUNDLE_ID:-net.neuralengine.cube}"
TEAM_ID="${TEAM_ID:-ZV8TTD4Z45}"
IDENTITY="${IDENTITY:-}"
PROFILE="${PROFILE:-}"
ENTITLEMENTS="${ENTITLEMENTS:-app/Cube/Resources/Cube.networking.entitlements}"

die() { echo "[sign-cube] ERROR: $*" >&2; exit 1; }

# 1. Ensure Cube.app exists
if [[ ! -d "$APP_DIR" || ! -f "$APP_DIR/Contents/MacOS/CubeBar" ]]; then
  echo "[sign-cube] Building $APP_DIR first..."
  make app
fi

if [[ ! -f "$ENTITLEMENTS" ]]; then
  die "missing $ENTITLEMENTS"
fi

if [[ ! -f "entitlements.networking.plist" ]]; then
  die "missing entitlements.networking.plist"
fi

# 2. Locate / build cube binary
CUBE_SRC="${CUBE_BIN:-${VMM_BIN:-}}"
if [[ -z "$CUBE_SRC" ]]; then
  if [[ -f "target/debug/cube" && -f "target/release/cube" ]]; then
    if [[ "target/debug/cube" -nt "target/release/cube" ]]; then
      CUBE_SRC="target/debug/cube"
    else
      CUBE_SRC="target/release/cube"
    fi
  elif [[ -f "target/debug/cube" ]]; then
    CUBE_SRC="target/debug/cube"
  elif [[ -f "target/release/cube" ]]; then
    CUBE_SRC="target/release/cube"
  else
    echo "[sign-cube] Building cube binary..."
    cargo build
    CUBE_SRC="target/debug/cube"
  fi
fi
echo "[sign-cube] Using cube binary: $CUBE_SRC"

# 3. Locate provisioning profile if not set
# Prefer Development profile for local execution over Distribution
if [[ -z "$PROFILE" ]]; then
  for candidate in "Cube_Dev.provisionprofile" "Cube_Dist.provisionprofile" "$ROOT"/*.provisionprofile; do
    if [[ -f "$candidate" ]]; then
      PROFILE="$candidate"
      echo "[sign-cube] Auto-detected provisioning profile: $PROFILE"
      break
    fi
  done
fi

if [[ -z "$PROFILE" || ! -f "$PROFILE" ]]; then
  die "Set PROFILE=/path/to/YourProfile.provisionprofile (file not found)"
fi

# 4. Detect signing identity that matches the selected profile
if [[ -z "$IDENTITY" ]]; then
  # Match certificates embedded in the profile with valid Keychain signing identities
  DETECTED="$(python3 -c "
import plistlib, hashlib, subprocess, re, sys

profile_path = '$PROFILE'
try:
    with open(profile_path, 'rb') as f:
        d = f.read()
    s = d.find(b'<?xml')
    e = d.find(b'</plist>')
    p = plistlib.loads(d[s:e+8])
    prof_hashes = [hashlib.sha1(c).hexdigest().upper() for c in p.get('DeveloperCertificates', [])]
except Exception as ex:
    prof_hashes = []

proc = subprocess.run(['security', 'find-identity', '-v', '-p', 'codesigning'], capture_output=True, text=True)
matched = None

# First priority: identity whose SHA1 matches a certificate embedded in the profile
for line in proc.stdout.splitlines():
    for sha in prof_hashes:
        if sha in line.upper():
            m = re.search(r'\"([^\"]+)\"', line)
            if m:
                matched = m.group(1)
                break
    if matched:
        break

# Second priority: Development or Developer ID identity matching TEAM_ID (never Apple Distribution for local run)
if not matched:
    for line in proc.stdout.splitlines():
        if '$TEAM_ID' in line and 'Apple Distribution' not in line:
            m = re.search(r'\"([^\"]+)\"', line)
            if m:
                matched = m.group(1)
                break

if matched:
    print(matched)
" 2>/dev/null || true)"

  if [[ -n "$DETECTED" ]]; then
    IDENTITY="$DETECTED"
    echo "[sign-cube] Auto-detected matching signing identity: $IDENTITY"
  else
    echo "[sign-cube] Available codesigning identities in keychain:"
    security find-identity -v -p codesigning || true
    die "Could not find a matching signing identity for profile $PROFILE. Set IDENTITY=\"Apple Development: ...\""
  fi
fi

# 5. Sanity-check profile contents (extract XML)
echo "[sign-cube] Validating provisioning profile: $PROFILE..."
PROF_XML="$(python3 -c "
with open('$PROFILE', 'rb') as f:
    d = f.read()
s = d.find(b'<?xml')
e = d.find(b'</plist>')
if s != -1 and e != -1:
    print(d[s:e+8].decode('utf-8', errors='ignore'))
" 2>/dev/null || true)"

if [[ -n "$PROF_XML" ]]; then
  if ! echo "$PROF_XML" | grep -q 'com.apple.vm.networking'; then
    echo "[sign-cube] WARNING: Profile does NOT contain 'com.apple.vm.networking'!" >&2
    echo "[sign-cube]          Unprivileged vmnet will fail launch constraints unless granted by Apple." >&2
  fi
  if ! echo "$PROF_XML" | grep -q "$BUNDLE_ID"; then
    echo "[sign-cube] WARNING: Profile does not seem to mention BUNDLE_ID=$BUNDLE_ID" >&2
  fi
fi

# 6. Embed provisioning profile
echo "[sign-cube] Embedding provisioning profile into $APP_DIR/Contents/embedded.provisionprofile..."
cp "$PROFILE" "$APP_DIR/Contents/embedded.provisionprofile"

# 7. Embed and sign cube as a helper app bundle inside Contents/Helpers/cube.app
# macOS AMFI requires that the executable being launched matches CFBundleExecutable
# in a bundle containing embedded.provisionprofile.
HELPER_APP="$APP_DIR/Contents/Helpers/cube.app"
mkdir -p "$HELPER_APP/Contents/MacOS"

cat > "$HELPER_APP/Contents/Info.plist" <<PLIST
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

cp "$CUBE_SRC" "$HELPER_APP/Contents/MacOS/cube"
chmod 755 "$HELPER_APP/Contents/MacOS/cube"
cp "$PROFILE" "$HELPER_APP/Contents/embedded.provisionprofile"

# Relative symlink in Contents/MacOS/cube pointing to the signed helper
ln -sf ../Helpers/cube.app/Contents/MacOS/cube "$APP_DIR/Contents/MacOS/cube"

echo "[sign-cube] Signing helper app $HELPER_APP..."
codesign --force \
  --sign "$IDENTITY" \
  --identifier "$BUNDLE_ID" \
  --entitlements entitlements.networking.plist \
  --options runtime \
  --timestamp \
  "$HELPER_APP"

# 8. Sign cubectl if present (standard hypervisor entitlements, avoiding restricted vm.networking which triggers AMFI SIGKILL on unbundled binaries)
if [[ -f "$APP_DIR/Contents/MacOS/cubectl" ]]; then
  echo "[sign-cube] Signing $APP_DIR/Contents/MacOS/cubectl..."
  codesign --force \
    --sign "$IDENTITY" \
    --identifier "$BUNDLE_ID.ctl" \
    --entitlements app/Cube/Resources/Cube.entitlements \
    --options runtime \
    --timestamp \
    "$APP_DIR/Contents/MacOS/cubectl"
fi

# 9. Sign main CubeBar executable
echo "[sign-cube] Signing $APP_DIR/Contents/MacOS/CubeBar..."
codesign --force \
  --sign "$IDENTITY" \
  --identifier "$BUNDLE_ID" \
  --entitlements "$ENTITLEMENTS" \
  --options runtime \
  --timestamp \
  "$APP_DIR/Contents/MacOS/CubeBar"

# 10. Sign outer app bundle (without --deep to avoid clobbering inner binaries)
echo "[sign-cube] Signing app bundle $APP_DIR..."
codesign --force \
  --sign "$IDENTITY" \
  --identifier "$BUNDLE_ID" \
  --entitlements "$ENTITLEMENTS" \
  --options runtime \
  --timestamp \
  "$APP_DIR"

# 10. Comprehensive Verification
echo "[sign-cube] Verifying bundle signatures..."
codesign --verify --verbose=2 "$APP_DIR"
codesign --verify --verbose=2 "$APP_DIR/Contents/MacOS/cube"

# Ensure TeamID is attached (not ad-hoc)
CUBE_TEAM="$(codesign -dv "$APP_DIR/Contents/MacOS/cube" 2>&1 | awk -F= '/TeamIdentifier=/ {print $2}')"
if [[ -z "$CUBE_TEAM" || "$CUBE_TEAM" == "not set" ]]; then
  die "Embedded cube is missing TeamIdentifier! It was signed ad-hoc, which will cause Launch Constraint Violations."
fi

echo "[sign-cube] cube TeamIdentifier: $CUBE_TEAM"
echo "[sign-cube] cube Entitlements:"
codesign -d --entitlements - --xml "$APP_DIR/Contents/MacOS/cube" 2>/dev/null | plutil -p - || true

echo "[sign-cube] CubeBar Entitlements:"
codesign -d --entitlements - --xml "$APP_DIR/Contents/MacOS/CubeBar" 2>/dev/null | plutil -p - || true

echo "[sign-cube] Success! Both Cube.app and embedded cube are signed with $IDENTITY (Team: $CUBE_TEAM)."
