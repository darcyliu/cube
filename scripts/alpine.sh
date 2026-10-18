#!/usr/bin/env bash
# ==============================================================================
# scripts/alpine.sh
#
# Alpine Linux ARM64 Virtual Machine Runner & Installer for macOS VMM
#
# Usage:
#   ./scripts/alpine.sh           # Runs installed VM, or starts live environment if first time
#   ./scripts/alpine.sh --install # Forces live ISO/initramfs boot with disk attached
# ==============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

TARGET_DISK="alpine.img"
DISK_SIZE="5g"
CPUS="2"
RAM_MB="1024"
FORCE_INSTALL=0
NET_MODE="vmnet-proxy"

# ANSI color codes
BOLD="\033[1m"
GREEN="\033[1;32m"
YELLOW="\033[1;33m"
RED="\033[1;31m"
CYAN="\033[1;36m"
RESET="\033[0m"

echo -e "${BOLD}${CYAN}======================================================${RESET}"
echo -e "${BOLD}${CYAN}  macOS Hypervisor: Alpine Linux ARM64 VM / Installer ${RESET}"
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
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -i, --install        Force live netboot / installer mode"
            echo "  -d, --disk <PATH>    Destination virtual disk path (default: alpine.img)"
            echo "  -s, --size <SIZE>    Target disk size (default: 5g, sparse)"
            echo "  -c, --cpus <N>       Number of virtual CPUs (default: 2)"
            echo "  -m, --ram <MB>       Physical RAM in MB (default: 1024)"
            echo "      --sudo           Use in-process vmnet with sudo instead of vmnet-proxy"
            exit 0
            ;;
        *)
            echo -e "${RED}[Error] Unknown argument: $1${RESET}"
            exit 1
            ;;
    esac
done

cd "$ROOT_DIR"

# 1. Download Alpine netboot media if missing
mkdir -p boot/alpine
if [[ ! -f boot/alpine/vmlinuz-virt ]]; then
    echo -e "${YELLOW}[Cube] Downloading Alpine Linux virt kernel...${RESET}"
    curl -fSL https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/aarch64/netboot/vmlinuz-virt -o boot/alpine/vmlinuz-virt
fi
if [[ ! -f boot/alpine/initramfs-virt ]]; then
    echo -e "${YELLOW}[Cube] Downloading Alpine Linux virt initramfs...${RESET}"
    curl -fSL https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/aarch64/netboot/initramfs-virt -o boot/alpine/initramfs-virt
fi
if [[ ! -f boot/alpine/modloop-virt ]]; then
    echo -e "${YELLOW}[Cube] Downloading Alpine Linux virt modloop...${RESET}"
    curl -fSL https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/aarch64/netboot/modloop-virt -o boot/alpine/modloop-virt
fi

# 2. Determine mode: Install or Run
IS_NEW_DISK=0
if [[ ! -f "$TARGET_DISK" ]]; then
    echo -e "${GREEN}[Cube] Creating sparse virtual disk '${TARGET_DISK}' (${DISK_SIZE})...${RESET}"
    truncate -s "$DISK_SIZE" "$TARGET_DISK"
    IS_NEW_DISK=1
fi

# 3. Ensure Cube binary is compiled and signed
VMM_BIN="${VMM_BIN:-target/debug/cube}"
if [[ ! -f "$VMM_BIN" ]]; then
    echo -e "${GREEN}[Cube] Building and signing Cube binary...${RESET}"
    cargo build
    codesign -s - --entitlements entitlements.plist --force "$VMM_BIN"
fi

# 4. Configure network mode
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
    echo -e "${GREEN}[Cube] Setting up macOS PF NAT forwarding on ${IF} for ${NAT_FROM}...${RESET}"
    sysctl -w net.inet.ip.forwarding=1 >/dev/null 2>&1 || true
    printf 'nat on %s from %s to any -> (%s)\npass in quick on bridge100 all\npass out quick on %s all\n' "$IF" "$NAT_FROM" "$IF" "$IF" | pfctl -a "com.apple/cube" -f - 2>/dev/null || true
fi

if [[ $IS_NEW_DISK -eq 1 || $FORCE_INSTALL -eq 1 ]]; then
    echo -e "\n${BOLD}${GREEN}======================================================${RESET}"
    echo -e "${BOLD}${GREEN}  Launching Alpine Live Environment                   ${RESET}"
    echo -e "${BOLD}${GREEN}======================================================${RESET}"
    echo -e "  Mode         : ${BOLD}Live / Installation${RESET}"
    echo -e "  Virtual Disk : ${BOLD}${TARGET_DISK}${RESET} (/dev/vda)"
    echo -e "  vCPUs / RAM  : ${CPUS} cores / ${RAM_MB} MB"
    echo -e "  Network      : ${BOLD}${NET_MODE}${RESET}"
    echo -e "  Instructions : Run '${BOLD}setup-alpine${RESET}' and choose disk '${BOLD}vda${RESET}' ('${BOLD}sys${RESET}')."
    echo -e "------------------------------------------------------\n"

    exec "$VMM_BIN" \
        --linux boot/alpine/vmlinuz-virt \
        --initrd boot/alpine/initramfs-virt \
        --bootargs "console=ttyAMA0 earlycon modules=loop,squashfs,sd-mod,virtio_net,virtio_mmio,virtio_blk ip=dhcp alpine_repo=https://dl-cdn.alpinelinux.org/alpine/latest-stable/main modloop=https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/aarch64/netboot/modloop-virt" \
        --disk "$TARGET_DISK" \
        --net "$NET_MODE" \
        --cpus "$CPUS" \
        --ram "$RAM_MB"
else
    echo -e "\n${BOLD}${GREEN}======================================================${RESET}"
    echo -e "${BOLD}${GREEN}  Booting Installed Alpine Virtual Machine            ${RESET}"
    echo -e "${BOLD}${GREEN}======================================================${RESET}"
    echo -e "  Mode         : ${BOLD}Installed System${RESET}"
    echo -e "  Virtual Disk : ${BOLD}${TARGET_DISK}${RESET} (/dev/vda)"
    echo -e "  vCPUs / RAM  : ${CPUS} cores / ${RAM_MB} MB"
    echo -e "  Network      : ${BOLD}${NET_MODE}${RESET}"
    echo -e "------------------------------------------------------\n"

    exec "$VMM_BIN" \
        --linux boot/alpine/vmlinuz-virt \
        --initrd boot/alpine/initramfs-virt \
        --bootargs "console=ttyAMA0 earlycon modules=loop,squashfs,sd-mod,virtio_net,virtio_mmio,virtio_blk" \
        --disk "$TARGET_DISK" \
        --net "$NET_MODE" \
        --cpus "$CPUS" \
        --ram "$RAM_MB"
fi
