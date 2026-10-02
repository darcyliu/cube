#!/usr/bin/env bash
# ==============================================================================
# scripts/debian.sh
#
# Debian 13 (Trixie) ARM64 Virtual Machine Runner & Installer for macOS VMM
#
# Modes:
#   make debian                 → Unattended install (preseed) on first run, boot after
#   INTERACTIVE=1 make debian   → Interactive Debian installer (classic menus)
#   make debian ARGS="--install" → Force reinstall (wipes disk)
# ==============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DISK="debian.img"
DISK_SIZE="10g"
CPUS="2"
RAM_MB="1024"
FORCE_INSTALL=0
INTERACTIVE="${INTERACTIVE:-0}"  # Set INTERACTIVE=1 to use classic installer menus
NET_MODE="vmnet-proxy"

# Disable IPv6 so d-i wget does not stall preferring AAAA records for deb.debian.org.
LINUX_NET_BOOTARGS="ipv6.disable=1 net.ifnames=0"
# Kernel cmdline copies of netcfg/mirror preseeds (marked "seen", so even the
# interactive installer configures DHCP and HTTP mirror cleanly). Keep in sync with debian-*.cfg.
NETCFG_BOOTARGS="netcfg/choose_interface=auto netcfg/get_hostname=debian netcfg/get_domain=local mirror/protocol=http mirror/country=manual mirror/http/hostname=deb.debian.org mirror/http/directory=/debian mirror/suite=trixie mirror/codename=trixie"

# ANSI color codes
BOLD="\033[1m"
GREEN="\033[1;32m"
YELLOW="\033[1;33m"
RED="\033[1;31m"
CYAN="\033[1;36m"
RESET="\033[0m"

echo -e "${BOLD}${CYAN}======================================================${RESET}"
echo -e "${BOLD}${CYAN}  macOS Hypervisor: Debian 13 (Trixie) ARM64 VM       ${RESET}"
echo -e "${BOLD}${CYAN}======================================================${RESET}\n"

# Parse CLI arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --install|-i)
            FORCE_INSTALL=1
            shift
            ;;
        --disk|-d)
            TARGET_DISK="$2"
            shift 2
            ;;
        --size|-s)
            DISK_SIZE="$2"
            shift 2
            ;;
        --cpus|-c)
            CPUS="$2"
            shift 2
            ;;
        --ram|-m)
            RAM_MB="$2"
            shift 2
            ;;
        --sudo)
            NET_MODE="vmnet"
            shift
            ;;
        --net)
            NET_MODE="$2"
            shift 2
            ;;
        --interactive|-I)
            INTERACTIVE=1
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -i, --install        Force netboot installation mode"
            echo "  -I, --interactive    Use classic interactive Debian installer (no preseed)"
            echo "  -d, --disk <PATH>    Destination virtual disk path (default: debian.img)"
            echo "  -s, --size <SIZE>    Target disk size (default: 10g, sparse)"
            echo "  -c, --cpus <N>       Number of virtual CPUs (default: 2)"
            echo "  -m, --ram <MB>       Physical RAM in MB (default: 1024)"
            echo "      --sudo           Use in-process vmnet with sudo instead of vmnet-proxy"
            echo "      --net <MODE>     Network mode: vmnet-proxy (default) or vmnet"
            echo ""
            echo "Environment:"
            echo "  INTERACTIVE=1        Same as --interactive"
            exit 0
            ;;
        *)
            echo -e "${RED}[Error] Unknown argument: $1${RESET}"
            exit 1
            ;;
    esac
done

cd "$ROOT_DIR"

# 1. Download Debian 13 Trixie netboot media if missing
mkdir -p boot/debian
KERNEL_PATH="boot/debian/linux-trixie"
INITRD_BASE="boot/debian/initrd-trixie.gz"

if [[ ! -f "$KERNEL_PATH" ]]; then
    echo -e "${YELLOW}[VMM] Downloading Debian 13 (Trixie) netboot kernel...${RESET}"
    curl -fSL https://deb.debian.org/debian/dists/trixie/main/installer-arm64/current/images/netboot/debian-installer/arm64/linux -o "$KERNEL_PATH"
fi
if [[ ! -f "$INITRD_BASE" ]]; then
    echo -e "${YELLOW}[VMM] Downloading Debian 13 (Trixie) netboot initrd...${RESET}"
    curl -fSL https://deb.debian.org/debian/dists/trixie/main/installer-arm64/current/images/netboot/debian-installer/arm64/initrd.gz -o "$INITRD_BASE"
fi

# 1b. Build preseed initrd (workspace root, user-writable, gitignored).
#     Appends a preseed.cfg cpio AFTER the original installer initrd so the
#     installer overlays it without affecting the boot process.
#     Interactive gets network+mirror only; unattended gets the full recipe.
INITRD_PRESEED="initrd-trixie-preseed.gz"
INITRD_NET_PRESEED="initrd-trixie-netpreseed.gz"
PRESEED_CFG="$SCRIPT_DIR/debian-preseed.cfg"
NET_PRESEED_CFG="$SCRIPT_DIR/debian-net-preseed.cfg"
if [[ "$INTERACTIVE" == "1" ]]; then
    if [[ ! -f "$INITRD_NET_PRESEED" || "$NET_PRESEED_CFG" -nt "$INITRD_NET_PRESEED" || "$SCRIPT_DIR/make-preseed-initrd.py" -nt "$INITRD_NET_PRESEED" ]]; then
        echo -e "${GREEN}[VMM] Building interactive network/mirror preseed initrd...${RESET}"
        python3 "$SCRIPT_DIR/make-preseed-initrd.py" "$INITRD_BASE" "$INITRD_NET_PRESEED" "$NET_PRESEED_CFG"
    fi
    INITRD_INSTALL="$INITRD_NET_PRESEED"
    PRESEED_CFG="$NET_PRESEED_CFG"
else
    if [[ ! -f "$INITRD_PRESEED" || "$PRESEED_CFG" -nt "$INITRD_PRESEED" || "$SCRIPT_DIR/make-preseed-initrd.py" -nt "$INITRD_PRESEED" ]]; then
        echo -e "${GREEN}[VMM] Building unattended preseed initrd...${RESET}"
        python3 "$SCRIPT_DIR/make-preseed-initrd.py" "$INITRD_BASE" "$INITRD_PRESEED" "$PRESEED_CFG"
    fi
    INITRD_INSTALL="$INITRD_PRESEED"
fi

# 2. Determine mode: Install or Run
IS_NEW_DISK=0
if [[ ! -f "$TARGET_DISK" ]]; then
    echo -e "${GREEN}[VMM] Creating sparse virtual disk '${TARGET_DISK}' (${DISK_SIZE})...${RESET}"
    truncate -s "$DISK_SIZE" "$TARGET_DISK"
    IS_NEW_DISK=1
else
    # A previous installer run that died at "Bad archive mirror" leaves an empty
    # sparse image with no GPT. Treat that as a fresh disk so we re-install.
    if ! python3 -c "import sys; f=open(sys.argv[1],'rb'); f.seek(512); sys.exit(0 if f.read(8)==b'EFI PART' else 1)" "$TARGET_DISK"; then
        echo -e "${YELLOW}[VMM] '${TARGET_DISK}' has no installed GPT — starting installer.${RESET}"
        IS_NEW_DISK=1
    fi
fi

# 3. Ensure VMM binary is compiled and signed
VMM_BIN="${VMM_BIN:-target/debug/vmm}"
if [[ ! -f "$VMM_BIN" ]]; then
    echo -e "${GREEN}[VMM] Building and signing VMM binary...${RESET}"
    cargo build
    codesign -s - --entitlements entitlements.plist --force "$VMM_BIN"
fi

# 4. Configure macOS PF NAT packet forwarding for guest internet access
if [[ $EUID -eq 0 ]]; then
    NET_MODE="vmnet"
    IF="$(netstat -rn -f inet 2>/dev/null | awk '$1=="default" && $2 !~ /link/ && $NF !~ /^bridge/ {print $NF; exit}')"
    if [[ -z "$IF" ]]; then
        IF="$(route get 1.1.1.1 2>/dev/null | awk '/interface:/{print $2}')"
    fi
    if [[ -z "$IF" || "$IF" == bridge* ]]; then
        IF="$(netstat -rn -f inet 2>/dev/null | awk '/default/{for(i=1;i<=NF;i++) if($i ~ /^en/) {print $i; exit}}')"
    fi
    IF="${IF:-en0}"
    BRIDGE_IP="$(ifconfig bridge100 2>/dev/null | awk '$1=="inet" {print $2; exit}')"
    BRIDGE_SUBNET="${BRIDGE_IP%.*}.0/24"
    if [[ -n "$BRIDGE_IP" && "$BRIDGE_SUBNET" != "192.168.18.0/24" ]]; then
        NAT_FROM="{ 192.168.18.0/24, ${BRIDGE_SUBNET} }"
    else
        NAT_FROM="192.168.18.0/24"
    fi
    echo -e "${GREEN}[VMM] Setting up macOS PF NAT forwarding on ${IF} for ${NAT_FROM}...${RESET}"
    sysctl -w net.inet.ip.forwarding=1 >/dev/null 2>&1 || true
    printf 'nat on %s from %s to any -> (%s)\npass in quick on bridge100 all\npass out quick on %s all\n' "$IF" "$NAT_FROM" "$IF" "$IF" | pfctl -a "com.apple/vmm" -f - 2>/dev/null || true
fi

if [[ $IS_NEW_DISK -eq 1 || $FORCE_INSTALL -eq 1 ]]; then
    if [[ "$INTERACTIVE" == "1" ]]; then
        echo -e "\n${BOLD}${GREEN}======================================================${RESET}"
        echo -e "${BOLD}${GREEN}  Starting Debian 13 (Trixie) Interactive Installer  ${RESET}"
        echo -e "${BOLD}${GREEN}======================================================${RESET}"
        echo -e "  Mode         : ${BOLD}Interactive${RESET} (classic installer menus)"
        echo -e "  Virtual Disk : ${BOLD}${TARGET_DISK}${RESET} (/dev/vda)"
        echo -e "  vCPUs / RAM  : ${CPUS} cores / ${RAM_MB} MB"
        echo -e "  Network      : ${BOLD}${NET_MODE}${RESET} (DHCP, DNS 1.1.1.1, HTTP deb.debian.org)"
        echo -e "  Tip          : Locale/users/disk stay interactive; network is preseeded"
        echo -e "  Debug        : Menu 2 (shell) then: cat /etc/resolv.conf; ping -c1 1.1.1.1"
        echo -e "------------------------------------------------------\n"
        "$VMM_BIN" \
            --linux "$KERNEL_PATH" \
            --initrd "$INITRD_INSTALL" \
            --bootargs "console=ttyAMA0 earlycon ${LINUX_NET_BOOTARGS} ${NETCFG_BOOTARGS}" \
            --disk "$TARGET_DISK" \
            --net "$NET_MODE" \
            --cpus "$CPUS" \
            --ram "${RAM_MB}" \
            --no-reboot
    else
        echo -e "\n${BOLD}${GREEN}======================================================${RESET}"
        echo -e "${BOLD}${GREEN}  Starting Debian 13 (Trixie) Unattended Installer   ${RESET}"
        echo -e "${BOLD}${GREEN}======================================================${RESET}"
        echo -e "  Mode         : ${BOLD}Unattended${RESET} (preseed — no input required)"
        echo -e "  Preseed      : ${BOLD}${PRESEED_CFG##*/}${RESET}"
        echo -e "  Virtual Disk : ${BOLD}${TARGET_DISK}${RESET} (/dev/vda)"
        echo -e "  vCPUs / RAM  : ${CPUS} cores / ${RAM_MB} MB"
        echo -e "  Network      : ${BOLD}${NET_MODE}${RESET}"
        echo -e "  Mirror       : deb.debian.org / trixie"
        echo -e "  Root login   : root:root after install"
        echo -e "  Tip          : Set INTERACTIVE=1 for classic menus"
        echo -e "------------------------------------------------------\n"
        # auto=true      → installer finds /preseed.cfg automatically
        # priority=critical → suppresses all non-critical questions
        "$VMM_BIN" \
            --linux "$KERNEL_PATH" \
            --initrd "$INITRD_INSTALL" \
            --bootargs "console=ttyAMA0 earlycon ${LINUX_NET_BOOTARGS} ${NETCFG_BOOTARGS} auto=true priority=critical DEBIAN_FRONTEND=text partman-basicfilesystems/no_swap=false" \
            --disk "$TARGET_DISK" \
            --net "$NET_MODE" \
            --cpus "$CPUS" \
            --ram "${RAM_MB}" \
            --no-reboot
    fi

    INSTALL_EXIT=$?
    if [[ $INSTALL_EXIT -ne 0 ]]; then
        echo -e "\n${RED}[Error] Installer exited with error code ${INSTALL_EXIT}.${RESET}"
        exit $INSTALL_EXIT
    fi

    echo -e "\n${BOLD}${GREEN}[VMM] Installation completed successfully.${RESET}"
    echo -e "${GREEN}[VMM] Preparing to boot the newly installed system...${RESET}\n"
    # Ensure freshly installed kernel/initrd are extracted rather than stale cached ones
    rm -f boot/debian/debian-installed-vmlinuz boot/debian/debian-installed-initrd.img boot/debian/debian-installed-rootpart
fi

# Boot the installed system: extract the kernel/initrd from the installed disk,
# like debian-cloud does. The netboot kernel cannot boot the installed system.
INSTALLED_KERNEL="boot/debian/debian-installed-vmlinuz"
INSTALLED_INITRD="boot/debian/debian-installed-initrd.img"
INSTALLED_ROOTPART="boot/debian/debian-installed-rootpart"
if [[ ! -f "$INSTALLED_KERNEL" || ! -f "$INSTALLED_INITRD" || ! -f "$INSTALLED_ROOTPART" ]]; then
    echo -e "${GREEN}[VMM] Extracting kernel/initrd from installed disk...${RESET}"
    python3 "$SCRIPT_DIR/extract-debian-cloud-kernel.py" \
        "$TARGET_DISK" "$INSTALLED_KERNEL" "$INSTALLED_INITRD" "$INSTALLED_ROOTPART" || {
        echo -e "${RED}[Error] Could not extract installed kernel.${RESET}"
        echo -e "${YELLOW}Hint: The installer may not have finished. Re-run make debian to install.${RESET}"
        exit 1
    }
fi

ROOT_DEV="$(cat "$INSTALLED_ROOTPART" 2>/dev/null || echo "/dev/vda1")"

echo -e "\n${BOLD}${GREEN}======================================================${RESET}"
echo -e "${BOLD}${GREEN}  Booting Installed Debian 13 Virtual Machine         ${RESET}"
echo -e "${BOLD}${GREEN}======================================================${RESET}"
echo -e "  Mode         : ${BOLD}Installed System${RESET}"
echo -e "  Virtual Disk : ${BOLD}${TARGET_DISK}${RESET} (/dev/vda)"
echo -e "  Root Dev     : ${BOLD}${ROOT_DEV}${RESET}"
echo -e "  Kernel       : ${BOLD}${INSTALLED_KERNEL##*/}${RESET} (extracted from disk)"
echo -e "  vCPUs / RAM  : ${CPUS} cores / ${RAM_MB} MB"
echo -e "  Network      : ${BOLD}${NET_MODE}${RESET}"
echo -e "------------------------------------------------------\n"

exec "$VMM_BIN" \
    --linux "$INSTALLED_KERNEL" \
    --initrd "$INSTALLED_INITRD" \
    --bootargs "console=ttyAMA0 earlycon root=${ROOT_DEV} rw ${LINUX_NET_BOOTARGS}" \
    --disk "$TARGET_DISK" \
    --net "$NET_MODE" \
    --cpus "$CPUS" \
    --ram "$RAM_MB"

