#!/usr/bin/env bash
# ==============================================================================
# scripts/clean-vms.sh
#
# Cleans generated FreeBSD VM instance files, seed images, overlays, and logs
# to allow recreating and testing fresh installations from the beginning.
#
# Preserves downloaded installation ISOs by default to avoid re-downloading.
# Pass --all to wipe downloaded installer media as well.
# ==============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "$WORKSPACE_DIR"

# ANSI colors
BOLD="\033[1m"
GREEN="\033[1;32m"
YELLOW="\033[1;33m"
CYAN="\033[1;36m"
RED="\033[1;31m"
RESET="\033[0m"

WIPE_ALL=0
AUTO_CONFIRM=0

# Parse arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --all|-a)
            WIPE_ALL=1
            shift
            ;;
        --yes|-y)
            AUTO_CONFIRM=1
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo
            echo "Options:"
            echo "  -y, --yes    Skip confirmation prompt and remove files immediately"
            echo "  -a, --all    Also remove downloaded FreeBSD installer ISOs & memstick images"
            echo "  -h, --help   Show this help message"
            exit 0
            ;;
        *)
            echo -e "${RED}[Error] Unknown option: $1${RESET}"
            exit 1
            ;;
    esac
done

echo -e "\n${BOLD}${CYAN}======================================================${RESET}"
echo -e "${BOLD}${CYAN}  VMM Instance Cleanup Tool                           ${RESET}"
echo -e "${BOLD}${CYAN}======================================================${RESET}"

# Check for running guest VMM processes (excluding background vmnet-service)
RUNNING_VMM=$(pgrep -f "target/(debug|release)/vmm" 2>/dev/null | while read -r pid; do
    if ! ps -p "$pid" -o args= 2>/dev/null | grep -q -- "--vmnet-service"; then
        echo "$pid"
    fi
done | tr '\n' ' ' | xargs)
if [[ -n "$RUNNING_VMM" ]]; then
    echo -e "${YELLOW}[Warning] Active guest VMM process detected (PID: ${RUNNING_VMM}).${RESET}"
    if [[ $AUTO_CONFIRM -eq 1 ]]; then
        echo -e "${YELLOW}Stopping running VMM instance...${RESET}"
        kill -9 $RUNNING_VMM 2>/dev/null || sudo kill -9 $RUNNING_VMM 2>/dev/null || true
    else
        read -p "Do you want to terminate the running VMM instance? [y/N]: " KILL_RESP
        if [[ "$KILL_RESP" =~ ^[Yy]$ ]]; then
            kill -9 $RUNNING_VMM 2>/dev/null || sudo kill -9 $RUNNING_VMM 2>/dev/null || true
            echo -e "${GREEN}VMM instance stopped.${RESET}"
        fi
    fi
fi

# Collect instance files to delete
ITEMS_TO_REMOVE=()

# 1. Virtual disk images
[[ -f "freebsd-installed.img" ]] && ITEMS_TO_REMOVE+=("freebsd-installed.img")
[[ -f "freebsd-unattended.img" ]] && ITEMS_TO_REMOVE+=("freebsd-unattended.img")
[[ -f "debian.img" ]] && ITEMS_TO_REMOVE+=("debian.img")
[[ -f "debian-cloud.img" ]] && ITEMS_TO_REMOVE+=("debian-cloud.img")
[[ -f "disk.raw" ]] && ITEMS_TO_REMOVE+=("disk.raw")
[[ -f "alpine.img" ]] && ITEMS_TO_REMOVE+=("alpine.img")

# 2. COW overlays, EFI variables, and temporary test images
for f in *.cow *.overlay *.tmp *.vars.fd; do
    [[ -f "$f" ]] && ITEMS_TO_REMOVE+=("$f")
done

# 3. Seed images, preseed initrds, and staging directories
[[ -f "seed.iso" ]] && ITEMS_TO_REMOVE+=("seed.iso")
[[ -f "cidata.iso" ]] && ITEMS_TO_REMOVE+=("cidata.iso")
[[ -f "initrd-trixie-preseed.gz" ]] && ITEMS_TO_REMOVE+=("initrd-trixie-preseed.gz")
[[ -f "initrd-trixie-netpreseed.gz" ]] && ITEMS_TO_REMOVE+=("initrd-trixie-netpreseed.gz")
[[ -d ".seed-staging" ]] && ITEMS_TO_REMOVE+=(".seed-staging")

# 4. Extracted boot kernels and installed system metadata
[[ -d ".iso-boot" ]] && ITEMS_TO_REMOVE+=(".iso-boot")
[[ -f "boot/debian/debian-installed-vmlinuz" ]] && ITEMS_TO_REMOVE+=("boot/debian/debian-installed-vmlinuz")
[[ -f "boot/debian/debian-installed-initrd.img" ]] && ITEMS_TO_REMOVE+=("boot/debian/debian-installed-initrd.img")
[[ -f "boot/debian/debian-installed-rootpart" ]] && ITEMS_TO_REMOVE+=("boot/debian/debian-installed-rootpart")
[[ -f "boot/debian/cloud-vmlinuz" ]] && ITEMS_TO_REMOVE+=("boot/debian/cloud-vmlinuz")
[[ -f "boot/debian/cloud-initrd.img" ]] && ITEMS_TO_REMOVE+=("boot/debian/cloud-initrd.img")
[[ -f "debian-cloud-vmlinuz" ]] && ITEMS_TO_REMOVE+=("debian-cloud-vmlinuz")
[[ -f "debian-cloud-initrd.img" ]] && ITEMS_TO_REMOVE+=("debian-cloud-initrd.img")

# 5. Downloaded images (only if --all)
if [[ $WIPE_ALL -eq 1 ]]; then
    [[ -f "mini-memstick.img" ]] && ITEMS_TO_REMOVE+=("mini-memstick.img")
    [[ -d "boot" ]] && ITEMS_TO_REMOVE+=("boot")
    for iso in FreeBSD*.iso; do
        [[ -f "$iso" ]] && ITEMS_TO_REMOVE+=("$iso")
    done
fi

# List files to be cleaned
if [[ ${#ITEMS_TO_REMOVE[@]} -eq 0 ]]; then
    echo -e "${GREEN}Workspace is already clean! No VM instance files found.${RESET}\n"
else
    echo -e "The following VM instance files will be removed:"
    for item in "${ITEMS_TO_REMOVE[@]}"; do
        if [[ -d "$item" ]]; then
            echo -e "  - ${RED}${item}/${RESET} (Directory)"
        else
            SIZE=$(ls -lh "$item" 2>/dev/null | awk '{print $5}')
            echo -e "  - ${RED}${item}${RESET} (${SIZE})"
        fi
    done

    if [[ $WIPE_ALL -eq 0 ]]; then
        echo -e "\n${YELLOW}[Note] Downloaded installer ISOs (FreeBSD*.iso) are preserved.${RESET}"
        echo -e "${YELLOW}       Pass '--all' to remove installer ISOs as well.${RESET}"
    fi

    # Confirmation
    if [[ $AUTO_CONFIRM -ne 1 ]]; then
        echo
        read -p "Proceed with deletion? [y/N]: " CONFIRM
        if [[ ! "$CONFIRM" =~ ^[Yy]$ ]]; then
            echo -e "${YELLOW}Cleanup cancelled.${RESET}\n"
            exit 0
        fi
    fi

    # Perform deletion
    echo -e "\n${GREEN}[Cube] Cleaning instance files...${RESET}"
    for item in "${ITEMS_TO_REMOVE[@]}"; do
        # Use sudo if permission denied (files might be owned by root from sudo truncate)
        rm -rf "$item" 2>/dev/null || sudo rm -rf "$item"
        echo -e "  ${GREEN}✓ Removed ${item}${RESET}"
    done
fi

# Clean up host /tmp pipes and logs
echo -e "${GREEN}[Cube] Cleaning host temporary files...${RESET}"
rm -f /tmp/vmm-uart-in \
      /tmp/vmm-unattended-console.log \
      /tmp/vmm-console.log \
      /tmp/vmm-net.log \
      /tmp/bsdinstall_log 2>/dev/null || true

echo -e "\n${BOLD}${GREEN}======================================================${RESET}"
echo -e "${BOLD}${GREEN}  Cleanup Complete! Workspace ready for fresh install ${RESET}"
echo -e "${BOLD}${GREEN}======================================================${RESET}"
echo -e "You can now test fresh installations from the beginning:"
echo -e "  - FreeBSD (Unattended Seed):   ${CYAN}make install-seed${RESET}"
echo -e "  - FreeBSD (Interactive ISO):   ${CYAN}make install-iso${RESET}"
echo -e "  - Debian 13 (Netboot Install): ${CYAN}make debian${RESET}"
echo -e "  - Debian 13 (Cloud Image):     ${CYAN}make debian-cloud${RESET}"
echo -e "  - Alpine Linux (Live/Install): ${CYAN}make alpine${RESET}\n"
