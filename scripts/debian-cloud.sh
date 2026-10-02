#!/usr/bin/env bash
# ==============================================================================
# scripts/debian-cloud.sh
#
# Fast Pre-Configured Debian 12 (Bookworm) ARM64 Cloud Virtual Machine
# Uses the official Debian Cloud "nocloud" ARM64 raw image (zero installer wizard)
#
# Usage:
#   ./scripts/debian-cloud.sh
#   make debian-cloud
# ==============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DISK="debian-cloud.img"
CPUS="2"
RAM_MB="1024"
NET_MODE="vmnet-proxy"
CLOUD_IMG_URL="https://cloud.debian.org/images/cloud/trixie/latest/debian-13-nocloud-arm64.tar.xz"
KERNEL_URL="https://deb.debian.org/debian/dists/trixie/main/installer-arm64/current/images/netboot/debian-installer/arm64/linux"

# ANSI color codes
BOLD="\033[1m"
GREEN="\033[1;32m"
YELLOW="\033[1;33m"
RED="\033[1;31m"
CYAN="\033[1;36m"
RESET="\033[0m"

echo -e "${BOLD}${CYAN}======================================================${RESET}"
echo -e "${BOLD}${CYAN}  Debian 13 (Trixie) ARM64 Cloud Virtual Machine      ${RESET}"
echo -e "${BOLD}${CYAN}======================================================${RESET}\n"

# Parse CLI arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --disk|-d)
            TARGET_DISK="$2"
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
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -d, --disk <PATH>    Virtual disk path (default: debian-cloud.img)"
            echo "  -c, --cpus <N>       Number of virtual CPUs (default: 2)"
            echo "  -m, --ram <MB>       Physical RAM in MB (default: 1024)"
            echo "      --sudo           Use in-process vmnet with sudo instead of vmnet-proxy"
            echo "      --net <MODE>     Network mode: vmnet-proxy (default) or vmnet"
            exit 0
            ;;
        *)
            echo -e "${RED}[Error] Unknown argument: $1${RESET}"
            exit 1
            ;;
    esac
done

cd "$ROOT_DIR"

# 1. Download and extract official pre-built Debian nocloud raw image if missing
mkdir -p boot/debian
if [[ ! -f "$TARGET_DISK" ]]; then
    echo -e "${YELLOW}[VMM] Downloading official Debian 13 'nocloud' ARM64 image...${RESET}"
    echo -e "${YELLOW}      Source: ${CLOUD_IMG_URL}${RESET}"
    curl -fSL --progress-bar "$CLOUD_IMG_URL" | tar -xJf -
    if [[ -f "disk.raw" ]]; then
        mv disk.raw "$TARGET_DISK"
    fi
    echo -e "${GREEN}[VMM] Pre-configured Debian 13 disk image ready: ${TARGET_DISK}${RESET}"
fi

# nocloud images lock root and run systemd-firstboot (easy to miss on serial).
# Always (re)prepare the image: unlock root, write machine-id, enable autologin.
echo -e "${GREEN}[VMM] Preparing nocloud image for serial login (root password: root)...${RESET}"
if ! python3 "$SCRIPT_DIR/patch-cloud-shadow.py" "$TARGET_DISK" root; then
    echo -e "${RED}[Error] Failed to prepare ${TARGET_DISK} for login.${RESET}"
    echo -e "${YELLOW}Hint: the image is often root-owned after 'sudo make debian-cloud'. Re-run with sudo.${RESET}"
    exit 1
fi


# 2. Extract distribution kernel and initramfs from the cloud image if missing
KERNEL_PATH="boot/debian/cloud-vmlinuz"
INITRD_PATH="boot/debian/cloud-initrd.img"

if [[ ! -f "$KERNEL_PATH" || ! -f "$INITRD_PATH" ]]; then
    if [[ -f "debian-cloud-vmlinuz" && -f "debian-cloud-initrd.img" ]]; then
        cp debian-cloud-vmlinuz "$KERNEL_PATH" 2>/dev/null || KERNEL_PATH="debian-cloud-vmlinuz"
        cp debian-cloud-initrd.img "$INITRD_PATH" 2>/dev/null || INITRD_PATH="debian-cloud-initrd.img"
    else
        echo -e "${YELLOW}[VMM] Extracting kernel and initramfs from ${TARGET_DISK}...${RESET}"
        python3 "$SCRIPT_DIR/extract-debian-cloud-kernel.py" "$TARGET_DISK" "$KERNEL_PATH" "$INITRD_PATH" || {
            KERNEL_PATH="debian-cloud-vmlinuz"
            INITRD_PATH="debian-cloud-initrd.img"
            python3 "$SCRIPT_DIR/extract-debian-cloud-kernel.py" "$TARGET_DISK" "$KERNEL_PATH" "$INITRD_PATH"
        }
    fi
fi

# 3. Create cloud-init NoCloud seed disk (cidata.iso) for unattended first-boot setup
# Always kept in workspace root (gitignored, never root-owned) for easy regeneration.
CIDATA_ISO="cidata.iso"

if [[ ! -f "$CIDATA_ISO" ]]; then
    echo -e "${GREEN}[VMM] Generating cloud-init NoCloud seed image (unattended)...${RESET}"

    # Pre-hash the root password so cloud-init applies it without interactive prompts
    ROOT_HASH="$(openssl passwd -6 -salt vmmdebian root 2>/dev/null || \
        printf '%s' '$6$vmmdebian$cTEwBCyPjJIDXBkb6D0Y/V9H8hJhcCWneA6klgFte2bqsmPQJ2jolV/JDKPAKdgyR0WxLRni1t7Asnlj2GI7w/')"

    CIDATA_DIR="$ROOT_DIR/scratch/cidata-$$"
    mkdir -p "$CIDATA_DIR"

    printf 'instance-id: debian-cloud-01\nlocal-hostname: debian\n' > "$CIDATA_DIR/meta-data"

    printf '%s\n' '#cloud-config' \
        '# Fully unattended: no prompts, no interactive modules' \
        'datasource_list: [NoCloud, None]' \
        'disable_root: false' \
        'users:' \
        '  - name: root' \
        "    hashed_passwd: \"$ROOT_HASH\"" \
        '    lock_passwd: false' \
        '    shell: /bin/bash' \
        'chpasswd:' \
        '  expire: false' \
        'ssh_pwauth: true' \
        'timezone: UTC' \
        'locale: en_US.UTF-8' \
        'keyboard:' \
        '  layout: us' \
        'growpart:' \
        '  mode: auto' \
        "  devices: ['/']" \
        'resize_rootfs: true' \
        'package_update: false' \
        'package_upgrade: false' \
        'runcmd:' \
        '  # Autologin on ttyAMA0 (serial console)' \
        '  - mkdir -p /etc/systemd/system/serial-getty@ttyAMA0.service.d' \
        '  - |' \
        '    printf '"'"'[Service]\nExecStart=\nExecStart=-/sbin/agetty -o "-p -- \\u" --noclear --autologin root %%I vt220\n'"'"' \' \
        '      > /etc/systemd/system/serial-getty@ttyAMA0.service.d/autologin.conf' \
        '  - systemctl daemon-reload' \
        '  - systemctl restart "serial-getty@ttyAMA0.service"' \
        "final_message: |" \
        "  Debian 13 (Trixie) Cloud VM ready." \
        "  Login: root  Password: root" \
        > "$CIDATA_DIR/user-data"

    hdiutil makehybrid -o "$CIDATA_ISO" -iso -joliet \
        -default-volume-name cidata "$CIDATA_DIR" >/dev/null 2>&1
    rm -rf "$CIDATA_DIR"
    echo -e "${GREEN}[VMM] Seed image ready: ${CIDATA_ISO} (root password: 'root')${RESET}"
fi


# 4. Ensure VMM binary is compiled and signed
VMM_BIN="${VMM_BIN:-target/debug/vmm}"
if [[ ! -f "$VMM_BIN" ]]; then
    echo -e "${GREEN}[VMM] Building and signing VMM binary...${RESET}"
    cargo build
    codesign -s - --entitlements entitlements.plist --force "$VMM_BIN"
fi

# 5. Configure macOS PF NAT packet forwarding for guest internet access
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

echo -e "\n${BOLD}${GREEN}======================================================${RESET}"
echo -e "${BOLD}${GREEN}  Booting Pre-configured Debian Cloud VM              ${RESET}"
echo -e "${BOLD}${GREEN}======================================================${RESET}"
echo -e "  Virtual Disk : ${BOLD}${TARGET_DISK}${RESET} (/dev/vda)"
echo -e "  Cloud Seed   : ${BOLD}${CIDATA_ISO}${RESET} (/dev/vdb)"
echo -e "  Kernel/Initrd: ${KERNEL_PATH} / ${INITRD_PATH}"
echo -e "  vCPUs / RAM  : ${CPUS} cores / ${RAM_MB} MB"
echo -e "  Network      : ${BOLD}${NET_MODE}${RESET}"
echo -e "  Login        : ${BOLD}root${RESET} (serial autologin; password 'root' if prompted)"
echo -e "------------------------------------------------------\n"

# systemd.firstboot=0: Debian 13 nocloud runs systemd-firstboot (hard to see on serial).
# ip=dhcp: nocloud has systemd-networkd; generator honors ip=dhcp.
# ipv6.disable=1: vmnet NAT is IPv4-only; AAAA lookups otherwise stall DNS.
exec "$VMM_BIN" \
    --linux "$KERNEL_PATH" \
    --initrd "$INITRD_PATH" \
    --bootargs "console=ttyAMA0 earlycon root=/dev/vda1 rw systemd.firstboot=0 ipv6.disable=1 net.ifnames=0 ip=dhcp systemd.unit=multi-user.target" \
    --disk "$TARGET_DISK" \
    --disk "$CIDATA_ISO" \
    --net "$NET_MODE" \
    --cpus "$CPUS" \
    --ram "$RAM_MB"

