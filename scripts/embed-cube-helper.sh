#!/bin/bash
set -euo pipefail

BINARY="${1:?missing binary path}"
APP="${2:?missing app path}"

CONTENTS="$APP/Contents"
HELPER="$CONTENTS/Helpers/cube.app/Contents"

mkdir -p "$HELPER/MacOS"

# Copy main Info.plist to helper and update keys
cp "$CONTENTS/Info.plist" "$HELPER/Info.plist"
/usr/bin/plutil -replace CFBundleExecutable -string "cube" "$HELPER/Info.plist"
/usr/bin/plutil -replace CFBundleName -string "cube" "$HELPER/Info.plist"
/usr/bin/plutil -remove CFBundleIconFile "$HELPER/Info.plist" 2>/dev/null || true
/usr/bin/plutil -remove CFBundleDisplayName "$HELPER/Info.plist" 2>/dev/null || true
/usr/bin/plutil -remove LSUIElement "$HELPER/Info.plist" 2>/dev/null || true

# Copy cube binary into helper bundle
cp -f "$BINARY" "$HELPER/MacOS/cube"
chmod 755 "$HELPER/MacOS/cube"

# Clear stale provisioning profiles from previous builds
rm -f "$CONTENTS/embedded.provisionprofile" "$HELPER/embedded.provisionprofile"

# Relative symlink for CLI access: Contents/MacOS/cube -> ../Helpers/cube.app/Contents/MacOS/cube
mkdir -p "$CONTENTS/MacOS"
ln -sf "../Helpers/cube.app/Contents/MacOS/cube" "$CONTENTS/MacOS/cube"

# Embed native cubectl CLI tool into Contents/MacOS/cubectl if present
CUBECTL_SRC="$(dirname "$BINARY")/cubectl"
if [ -f "$CUBECTL_SRC" ]; then
    cp -f "$CUBECTL_SRC" "$CONTENTS/MacOS/cubectl"
    chmod 755 "$CONTENTS/MacOS/cubectl"
fi

# Embed UEFI firmware and licenses into Contents/Resources if present
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
if [ -f "$REPO_ROOT/firmware/QEMU_EFI.fd" ]; then
    mkdir -p "$CONTENTS/Resources/firmware"
    cp -f "$REPO_ROOT/firmware/QEMU_EFI.fd" "$CONTENTS/Resources/firmware/QEMU_EFI.fd"
fi

if [ -f "$REPO_ROOT/firmware/LICENSE.txt" ]; then
    mkdir -p "$CONTENTS/Resources/firmware" "$CONTENTS/Resources/Licenses"
    cp -f "$REPO_ROOT/firmware/LICENSE.txt" "$CONTENTS/Resources/firmware/LICENSE.txt"
    cp -f "$REPO_ROOT/firmware/LICENSE.txt" "$CONTENTS/Resources/Licenses/EDK2-LICENSE.txt"
fi

if [ -f "$REPO_ROOT/app/Cube/Resources/Credits.rtf" ]; then
    cp -f "$REPO_ROOT/app/Cube/Resources/Credits.rtf" "$CONTENTS/Resources/Credits.rtf"
fi

