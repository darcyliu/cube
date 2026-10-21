#!/bin/bash
# ==============================================================================
# scripts/create-dmg.sh - Build, Package, Sign, and Notarize Cube DMG
# ==============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Formatting and logging
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m'

log_info()    { echo -e "${BLUE}${BOLD}==>${NC} ${BOLD}$1${NC}"; }
log_success() { echo -e "${GREEN}${BOLD}✔${NC} $1"; }
log_warn()    { echo -e "${YELLOW}${BOLD}⚠${NC} $1"; }
log_error()   { echo -e "${RED}${BOLD}✖${NC} $1" >&2; }

# Default configuration
APP_PATH="dist/Cube.app"
OUTPUT_DMG="dist/Cube.dmg"
VOL_NAME="Cube"
SIGNING_IDENTITY="${SIGNING_IDENTITY:-${IDENTITY:-}}"
NOTARY_PROFILE="${NOTARY_PROFILE:-${NOTARYTOOL_PROFILE:-Cube-Profile}}"
SKIP_NOTARIZE=false

# Parse flags or positional arguments
POSITIONAL_ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        -p|--notary-profile)
            NOTARY_PROFILE="$2"
            shift 2
            ;;
        -i|--identity)
            SIGNING_IDENTITY="$2"
            shift 2
            ;;
        --skip-notarize)
            SKIP_NOTARIZE=true
            shift
            ;;
        -h|--help)
            cat <<EOF
Usage: $(basename "$0") [options] [APP_PATH] [OUTPUT_DMG] [VOLUME_NAME]

Packages Cube.app into a modern macOS drag-and-drop DMG, signs it, and
optionally notarizes with Apple Notary Service.

Options:
    -p, --notary-profile <NAME>  Keychain profile for xcrun notarytool (default: Cube-Profile).
                                 (or set NOTARY_PROFILE env var)
    -i, --identity <NAME>        Code signing identity (Developer ID Application).
                                 (or set IDENTITY env var)
    --skip-notarize              Skip notarization submission.
    -h, --help                   Show this help message.

EOF
            exit 0
            ;;
        *)
            POSITIONAL_ARGS+=("$1")
            shift
            ;;
    esac
done

if [ ${#POSITIONAL_ARGS[@]} -ge 1 ]; then
    APP_PATH="${POSITIONAL_ARGS[0]}"
fi
if [ ${#POSITIONAL_ARGS[@]} -ge 2 ]; then
    OUTPUT_DMG="${POSITIONAL_ARGS[1]}"
fi
if [ ${#POSITIONAL_ARGS[@]} -ge 3 ]; then
    VOL_NAME="${POSITIONAL_ARGS[2]}"
fi

ICON_PATH="$REPO_ROOT/app/Cube/Resources/AppIcon.icns"

if [ ! -d "$APP_PATH" ]; then
    log_error "App bundle not found at '$APP_PATH'."
    echo "Please build the app first (e.g. 'make app')." >&2
    exit 1
fi

# Validate the app itself before creating or replacing any DMG. Container signing
# and notarization cannot repair an app/profile certificate mismatch.
python3 "$SCRIPT_DIR/validate-signing.py" --app "$APP_PATH"
if [ -z "$SIGNING_IDENTITY" ]; then
    SIGNING_IDENTITY=$(codesign -dvv "$APP_PATH" 2>&1 | sed -n 's/^Authority=//p' | head -n 1)
fi

mkdir -p "$(dirname "$OUTPUT_DMG")"
rm -f "$OUTPUT_DMG"

WORK_DIR="$REPO_ROOT/target"
mkdir -p "$WORK_DIR"
TMP_DIR=$(mktemp -d "$WORK_DIR/cube-dmg-build.XXXXXX")
TMP_DMG="$TMP_DIR/pack.temp.dmg"
DEV_NAME=""

cleanup() {
    local status=$?
    if [ -n "$DEV_NAME" ]; then
        if command -v diskutil >/dev/null 2>&1; then
            diskutil eject "$DEV_NAME" >/dev/null 2>&1 || true
        else
            hdiutil detach "$DEV_NAME" -force >/dev/null 2>&1 || true
        fi
    fi
    rm -rf "$TMP_DIR"
    exit $status
}
trap cleanup EXIT INT TERM

# Calculate required size in MB (+ 60MB padding for filesystem overhead & layout)
APP_SIZE_MB=$(du -sm "$APP_PATH" | cut -f1)
TOTAL_SIZE_MB=$((APP_SIZE_MB + 60))

# ------------------------------------------------------------------------------
# 1. Create Staging Disk Image (using diskutil image to avoid deprecation warnings)
# ------------------------------------------------------------------------------
log_info "Creating staging disk image (${TOTAL_SIZE_MB} MB)..."
if command -v diskutil >/dev/null 2>&1 && diskutil image create blank --help >/dev/null 2>&1; then
    diskutil image create blank --size "${TOTAL_SIZE_MB}m" --volumeName "$VOL_NAME" "$TMP_DMG" >/dev/null
else
    hdiutil create -size "${TOTAL_SIZE_MB}m" -fs HFS+ -volname "$VOL_NAME" -ov "$TMP_DMG" >/dev/null
fi

# Unmount any existing /Volumes/$VOL_NAME volume to prevent name collision (e.g. "Cube 1")
if [ -d "/Volumes/$VOL_NAME" ]; then
    log_warn "Existing volume found at /Volumes/$VOL_NAME. Detaching..."
    diskutil eject "/Volumes/$VOL_NAME" >/dev/null 2>&1 || hdiutil detach "/Volumes/$VOL_NAME" -force >/dev/null 2>&1 || true
fi

log_info "Mounting staging volume..."
if command -v diskutil >/dev/null 2>&1 && diskutil image attach --help >/dev/null 2>&1; then
    MOUNT_INFO=$(diskutil image attach "$TMP_DMG")
else
    MOUNT_INFO=$(hdiutil attach -readwrite -noverify -noautoopen "$TMP_DMG")
fi

DEV_NAME=$(echo "$MOUNT_INFO" | grep '^/dev/' | head -n1 | awk '{print $1}')
MOUNT_DIR=$(echo "$MOUNT_INFO" | grep -o '/Volumes/.*' | head -n1)

if [ -z "$DEV_NAME" ] || [ -z "$MOUNT_DIR" ] || [ ! -d "$MOUNT_DIR" ]; then
    log_error "Failed to attach temporary disk image!"
    exit 1
fi

VOLUME_NAME=$(basename "$MOUNT_DIR")

log_info "Copying $(basename "$APP_PATH") and creating /Applications symlink..."
cp -a "$APP_PATH" "$MOUNT_DIR/"
ln -s /Applications "$MOUNT_DIR/Applications"

# Set volume icon if available
if [ -f "$ICON_PATH" ]; then
    cp -f "$ICON_PATH" "$MOUNT_DIR/.VolumeIcon.icns"
    if command -v SetFile >/dev/null 2>&1; then
        SetFile -c icnC "$MOUNT_DIR/.VolumeIcon.icns" 2>/dev/null || true
        SetFile -a C "$MOUNT_DIR" 2>/dev/null || true
    fi
    chflags -h hidden "$MOUNT_DIR/.VolumeIcon.icns" 2>/dev/null || true
fi

# ------------------------------------------------------------------------------
# 2. Configure Finder Presentation via AppleScript
# ------------------------------------------------------------------------------
APP_NAME="$(basename "$APP_PATH")"
if osascript -e 'tell application "Finder" to get version' >/dev/null 2>&1; then
    log_info "Configuring Finder window layout for '${VOLUME_NAME}'..."
    osascript <<EOF >/dev/null 2>&1 || log_warn "Finder configuration encountered a minor issue; continuing..."
tell application "Finder"
    tell disk "$VOLUME_NAME"
        open
        delay 1
        set current view of container window to icon view
        set toolbar visible of container window to false
        set statusbar visible of container window to false
        set the bounds of container window to {400, 200, 940, 560}
        set viewOptions to the icon view options of container window
        set icon size of viewOptions to 120
        set text size of viewOptions to 14
        set arrangement of viewOptions to not arranged
        set position of item "$APP_NAME" of container window to {140, 160}
        set position of item "Applications" of container window to {400, 160}
        close
        open
        update without registering applications
        delay 1
    end tell
end tell
EOF
else
    log_warn "Finder is not available in this environment. Skipping visual layout customization."
fi

# Finalize filesystem flags
sync
sleep 1

# ------------------------------------------------------------------------------
# 3. Unmount Staging Volume
# ------------------------------------------------------------------------------
log_info "Unmounting staging volume..."
if command -v diskutil >/dev/null 2>&1; then
    diskutil eject "$DEV_NAME" >/dev/null 2>&1 || true
else
    hdiutil detach "$DEV_NAME" -force >/dev/null 2>&1 || true
fi
DEV_NAME=""

# ------------------------------------------------------------------------------
# 4. Convert into Final Compressed Read-Only DMG (UDZO)
# ------------------------------------------------------------------------------
log_info "Compressing into final read-only DMG (${OUTPUT_DMG})..."
if command -v diskutil >/dev/null 2>&1 && diskutil image create from --help >/dev/null 2>&1; then
    diskutil image create from --format UDZO "$TMP_DMG" "$OUTPUT_DMG" >/dev/null
else
    hdiutil convert "$TMP_DMG" -format UDZO -imagekey zlib-level=9 -o "$OUTPUT_DMG" >/dev/null
fi
rm -f "$TMP_DMG"

# ------------------------------------------------------------------------------
# 5. Sign DMG Container
# ------------------------------------------------------------------------------
if [ -n "$SIGNING_IDENTITY" ]; then
    log_info "Signing DMG container with timestamp (${SIGNING_IDENTITY})..."
    codesign --force --timestamp --sign "$SIGNING_IDENTITY" "$OUTPUT_DMG"
    log_success "DMG signed successfully."
else
    log_warn "No signing identity specified. Skipping DMG container signing."
fi

# ------------------------------------------------------------------------------
# 6. Notarize and Staple DMG (Reference: Cheetah package_app.sh)
# ------------------------------------------------------------------------------
if [ "$SKIP_NOTARIZE" = false ] && [ -n "$NOTARY_PROFILE" ]; then
    log_info "Submitting DMG to Apple Notary Service (profile: ${NOTARY_PROFILE})..."
    SUBMISSION_LOG="$WORK_DIR/notary_submit.log"
    xcrun notarytool submit "$OUTPUT_DMG" \
        --keychain-profile "$NOTARY_PROFILE" \
        --wait 2>&1 | tee "$SUBMISSION_LOG"

    if grep -q "status: Accepted" "$SUBMISSION_LOG"; then
        log_success "Apple Notary Service accepted the submission."
    else
        log_error "Apple Notary Service rejected the submission!"
        SUBMISSION_ID=$(grep -E '^\s*id: [a-f0-9-]+' "$SUBMISSION_LOG" | head -n1 | awk '{print $2}')
        if [ -n "$SUBMISSION_ID" ]; then
            log_info "Fetching submission error log from Apple (id: $SUBMISSION_ID)..."
            xcrun notarytool log "$SUBMISSION_ID" --keychain-profile "$NOTARY_PROFILE" || true
        fi
        exit 1
    fi

    log_info "Stapling notarization ticket to DMG..."
    xcrun stapler staple "$OUTPUT_DMG"
    xcrun stapler validate "$OUTPUT_DMG"
    log_success "Notarization ticket stapled and validated successfully."

    log_info "Validating Gatekeeper acceptance..."
    spctl -a -t open --context context:primary-signature -v "$OUTPUT_DMG"
    log_success "Gatekeeper validation passed!"

    # Simulate quarantined first-launch Gatekeeper assessment
    log_info "Verifying quarantined first-launch assessment..."
    VERIFY_DIR="$(mktemp -d "$WORK_DIR/verify.XXXXXX")"
    VERIFY_MOUNT="$VERIFY_DIR/mount"
    mkdir -p "$VERIFY_MOUNT"
    
    if command -v diskutil >/dev/null 2>&1 && diskutil image attach --help >/dev/null 2>&1; then
        diskutil image attach --readOnly --nobrowse --mountPoint "$VERIFY_MOUNT" "$OUTPUT_DMG" >/dev/null 2>&1 || true
    else
        hdiutil attach -readonly -nobrowse -noverify -noautoopen -mountpoint "$VERIFY_MOUNT" "$OUTPUT_DMG" >/dev/null 2>&1 || true
    fi

    if [ -d "$VERIFY_MOUNT/$APP_NAME" ]; then
        cp -R "$VERIFY_MOUNT/$APP_NAME" "$VERIFY_DIR/$APP_NAME"
        if command -v diskutil >/dev/null 2>&1; then
            diskutil eject "$VERIFY_MOUNT" >/dev/null 2>&1 || true
        else
            hdiutil detach "$VERIFY_MOUNT" -force >/dev/null 2>&1 || true
        fi

        xattr -w com.apple.quarantine "0081;$(printf '%x' "$(date +%s)");Safari;" "$VERIFY_DIR/$APP_NAME"
        if spctl -a -t exec -vv "$VERIFY_DIR/$APP_NAME" 2>&1 | tee "$VERIFY_DIR/spctl.txt" | grep -q "source=Notarized Developer ID"; then
            log_success "Quarantined app accepted by Gatekeeper (Notarized Developer ID)."
        else
            log_warn "Quarantine assessment result:"
            cat "$VERIFY_DIR/spctl.txt" >&2 || true
        fi
    fi
    rm -rf "$VERIFY_DIR"
elif [ "$SKIP_NOTARIZE" = false ]; then
    log_warn "No notary profile specified (use --notary-profile <NAME> or set NOTARY_PROFILE). Skipping Apple notarization."
fi

# ------------------------------------------------------------------------------
# Summary
# ------------------------------------------------------------------------------
DMG_SIZE=$(du -sh "$OUTPUT_DMG" | cut -f1 | xargs)
echo ""
echo -e "${GREEN}${BOLD}======================================================${NC}"
echo -e "${GREEN}${BOLD}    Cube DMG Package Created Successfully!            ${NC}"
echo -e "${GREEN}${BOLD}======================================================${NC}"
echo -e "  ${BOLD}Artifact:${NC}  ${OUTPUT_DMG}"
echo -e "  ${BOLD}Size:${NC}      ${DMG_SIZE}"
if [ -n "$SIGNING_IDENTITY" ]; then
    echo -e "  ${BOLD}Signed with:${NC} ${SIGNING_IDENTITY}"
fi
if [ -n "$NOTARY_PROFILE" ]; then
    echo -e "  ${BOLD}Notarized:${NC}   Yes (profile: ${NOTARY_PROFILE})"
fi
echo ""
