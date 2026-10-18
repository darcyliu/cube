#!/usr/bin/env bash
# ==============================================================================
# scripts/install-from-iso.sh
#
# Automated Installer & Boot Tool for FreeBSD from an Installation ISO
# For macOS ARM64 Hypervisor VMM
# ==============================================================================

set -e

# Default settings
DEFAULT_ISO_URL="https://download.freebsd.org/releases/arm64/aarch64/ISO-IMAGES/14.2/FreeBSD-14.2-RELEASE-arm64-aarch64-disc1.iso"
DEFAULT_ISO="FreeBSD-14.2-RELEASE-arm64-aarch64-disc1.iso"
TARGET_DISK="freebsd-installed.img"
DISK_SIZE="20g"
CPUS="2"
RAM_MB="1024"
ISO_PATH=""
CPU_MODEL=""
VMM_BIN="./target/debug/cube"
USE_SUDO=false

# ANSI color codes
BOLD="\033[1m"
GREEN="\033[1;32m"
YELLOW="\033[1;33m"
RED="\033[1;31m"
CYAN="\033[1;36m"
RESET="\033[0m"

echo -e "${BOLD}${CYAN}======================================================${RESET}"
echo -e "${BOLD}${CYAN}  macOS Hypervisor: FreeBSD ISO Installation Tool      ${RESET}"
echo -e "${BOLD}${CYAN}======================================================${RESET}\n"

# Parse CLI arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --iso|-i)
            ISO_PATH="$2"
            shift 2
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
        --cpu-model)
            CPU_MODEL="$2"
            shift 2
            ;;
        --vmm)
            VMM_BIN="$2"
            shift 2
            ;;
        --sudo)
            USE_SUDO=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -i, --iso <PATH>        Path to FreeBSD ARM64 ISO image (disc1.iso or bootonly.iso)"
            echo "  -d, --disk <PATH>       Destination virtual disk path (default: freebsd-installed.img)"
            echo "  -s, --size <SIZE>       Target disk size (default: 20g, sparse)"
            echo "  -c, --cpus <N>          Number of virtual CPUs (default: 2)"
            echo "  -m, --ram <MB>          Physical RAM in MB (default: 1024)"
            echo "      --cpu-model <MODEL> CPU model/MIDR spoofing (e.g. host, m2, neoverse-n2, 0x<hex>)"
            echo "      --vmm <PATH>        Cube binary (default: ./target/debug/cube)"
            echo "      --sudo              In-process vmnet as root (default: vmnet-proxy)"
            echo "  -h, --help              Show this help message"
            exit 0
            ;;
        *)
            if [[ -z "$ISO_PATH" && -f "$1" ]]; then
                ISO_PATH="$1"
                shift
            else
                echo -e "${RED}[Error] Unknown argument: $1${RESET}"
                exit 1
            fi
            ;;
    esac
done

# Step 1: Resolve ISO image path or download official ARM64 ISO
if [[ -z "$ISO_PATH" ]]; then
    LOCAL_ISO=$(ls FreeBSD*arm64*.iso 2>/dev/null | head -n 1 || true)
    if [[ -n "$LOCAL_ISO" && -f "$LOCAL_ISO" ]]; then
        ISO_PATH="$LOCAL_ISO"
        echo -e "${GREEN}[Cube] Auto-detected local ARM64 ISO: ${ISO_PATH}${RESET}"
    elif [[ -f "$DEFAULT_ISO" ]]; then
        ISO_PATH="$DEFAULT_ISO"
        echo -e "${GREEN}[Cube] Found local ISO: ${ISO_PATH}${RESET}"
    else
        echo -e "${YELLOW}[Cube] No ARM64 ISO found. Downloading official FreeBSD ARM64 disc1 ISO...${RESET}"
        echo -e "${CYAN}URL: ${DEFAULT_ISO_URL}${RESET}"
        curl -fSL --progress-bar -o "$DEFAULT_ISO" "$DEFAULT_ISO_URL"
        ISO_PATH="$DEFAULT_ISO"
    fi
fi

if [[ ! -f "$ISO_PATH" ]]; then
    echo -e "${RED}[Error] ISO file not found: ${ISO_PATH}${RESET}"
    exit 1
fi

# Step 2: Architecture verification
echo -e "${GREEN}[Cube] Verifying ISO image architecture...${RESET}"

# Create temporary directory for kernel extraction
EXTRACT_DIR=".iso-boot"
mkdir -p "$EXTRACT_DIR"

# Extract kernel directly using macOS bsdtar (no mounting required)
echo -e "${GREEN}[Cube] Extracting kernel from ISO via bsdtar (no root required)...${RESET}"
if ! tar -xf "$ISO_PATH" -C "$EXTRACT_DIR" boot/kernel/kernel 2>/dev/null; then
    echo -e "${RED}[Error] Could not find 'boot/kernel/kernel' inside ${ISO_PATH}.${RESET}"
    echo -e "${YELLOW}Please ensure this is a standard FreeBSD installation ISO.${RESET}"
    exit 1
fi
EXTRACTED_KERNEL="${EXTRACT_DIR}/boot/kernel/kernel"

# Same ISO also ships zfs.ko. Cache it next to the kernel so a later Auto (ZFS)
# install can boot. This is not a filesystem choice; UFS ignores the file.
mkdir -p boot/kernel
if tar -xf "$ISO_PATH" -C . boot/kernel/zfs.ko 2>/dev/null; then
    echo -e "${GREEN}[Cube] Cached boot/kernel/zfs.ko from ISO (used only if the disk GPT is ZFS).${RESET}"
fi

# Inspect architecture with file
KERNEL_INFO=$(file "$EXTRACTED_KERNEL")
echo -e "${CYAN}[Cube] Kernel info: ${KERNEL_INFO}${RESET}"

if [[ "$KERNEL_INFO" != *"ARM aarch64"* && "$KERNEL_INFO" != *"aarch64"* ]]; then
    echo -e "${RED}[Error] Architecture mismatch!${RESET}"
    echo -e "${RED}The provided ISO is not for ARM64 (AArch64).${RESET}"
    echo -e "${YELLOW}Notice: Apple Silicon requires an ARM64 (aarch64) FreeBSD kernel.${RESET}"
    echo -e "${YELLOW}Detected binary type: ${KERNEL_INFO}${RESET}"
    exit 1
fi

# Step 3: Create sparse target virtual disk if not present
if [[ ! -f "$TARGET_DISK" ]]; then
    echo -e "${GREEN}[Cube] Creating sparse destination disk '${TARGET_DISK}' (${DISK_SIZE})...${RESET}"
    truncate -s "$DISK_SIZE" "$TARGET_DISK"
else
    echo -e "${YELLOW}[Cube] Using existing destination disk '${TARGET_DISK}' (${DISK_SIZE}).${RESET}"
fi

# Step 4: Ensure Cube binary is compiled and signed
if [[ ! -f "$VMM_BIN" ]]; then
    if [[ "$VMM_BIN" == "./target/debug/cube" ]]; then
        echo -e "${GREEN}[Cube] Building Cube binary...${RESET}"
        cargo build
        codesign -s - --entitlements entitlements.plist --force target/debug/cube
    else
        echo -e "${RED}[Error] Cube binary does not exist: ${VMM_BIN}${RESET}"
        exit 1
    fi
fi

# Step 5: Boot Installer
echo -e "\n${BOLD}${GREEN}======================================================${RESET}"
echo -e "${BOLD}${GREEN}  Launching FreeBSD Installer via VMM                 ${RESET}"
echo -e "${BOLD}${GREEN}======================================================${RESET}"
echo -e "  Target Disk : ${BOLD}${TARGET_DISK}${RESET} (/dev/vtbd0)"
echo -e "  CD-ROM ISO  : ${BOLD}${ISO_PATH}${RESET} (/dev/vtbd1)"
echo -e "  Boot Kernel : ${BOLD}${EXTRACTED_KERNEL}${RESET}"
echo -e "  Root Device : ${BOLD}cd9660:/dev/vtbd1${RESET}"
echo -e "  vCPUs / RAM : ${CPUS} vCPUs, ${RAM_MB} MB"
echo -e "------------------------------------------------------"
echo -e "${YELLOW}Inside the installer:${RESET}"
echo -e "  1. Select 'Install' and proceed with standard bsdinstall."
echo -e "  2. When prompted for partitioning, select ${BOLD}Auto (UFS)${RESET} on ${BOLD}/dev/vtbd0${RESET}"
echo -e "     (recommended). ${BOLD}Auto (ZFS)${RESET} works if ${BOLD}boot/kernel/zfs.ko${RESET} is present"
echo -e "     (run ${CYAN}make fetch-freebsd${RESET}); the VMM preloads it and boots ${CYAN}zfs:zroot/ROOT/default${RESET}."
echo -e "  3. When configuring Network: select ${BOLD}vtnet0${RESET} -> IPv4 -> ${BOLD}DHCP${RESET}."
echo -e "  4. At 'Manual Configuration' before exit, choose ${BOLD}Yes${RESET} (Shell) and run:"
echo -e "     ${CYAN}echo 'supersede domain-name-servers 1.1.1.1, 8.8.8.8;' >> /etc/dhclient.conf${RESET}"
echo -e "     ${CYAN}echo 'nameserver 1.1.1.1' > /etc/resolv.conf${RESET}"
echo -e "     ${CYAN}echo 'nameserver 8.8.8.8' >> /etc/resolv.conf${RESET}"
echo -e "     ${CYAN}sysrc defaultrouter=\"192.168.18.1\"${RESET}"
echo -e "     ${CYAN}route add default 192.168.18.1${RESET}   # sysrc is not live; needed for this session"
echo -e "     ${CYAN}sysrc ifconfig_vtnet0=\"DHCP\"${RESET}"
echo -e "     ${CYAN}exit${RESET}"
echo -e "  5. When complete, choose 'Reboot' or press ${BOLD}Ctrl-]${RESET} to exit."
echo -e "------------------------------------------------------\n"

CPU_MODEL_ARGS=()
if [[ -n "$CPU_MODEL" ]]; then
    CPU_MODEL_ARGS=(--cpu-model "$CPU_MODEL")
fi

# In-process vmnet installs its own PF NAT. The proxy path uses the service.
NET_MODE="vmnet-proxy"
VMM_CMD=("$VMM_BIN")
if [[ $USE_SUDO == true ]]; then
    NET_MODE="vmnet"
    VMM_CMD=(sudo "$VMM_BIN")
    IF=$(netstat -rn -f inet 2>/dev/null | awk '/default/{print $4; exit}')
    if [[ -n "$IF" ]]; then
        sudo sysctl -w net.inet.ip.forwarding=1 >/dev/null 2>&1 || true
        printf 'nat on %s from 192.168.18.0/24 to any -> (%s)\npass in quick on bridge100 all\npass out quick on %s all\n' "$IF" "$IF" "$IF" | sudo pfctl -a "com.apple/cube" -f - 2>/dev/null || true
    fi
fi

"${VMM_CMD[@]}" \
    --freebsd "$EXTRACTED_KERNEL" \
    --disk "$TARGET_DISK" \
    --cdrom "$ISO_PATH" \
    --root "cd9660:/dev/vtbd1" \
    --multiuser \
    --net "$NET_MODE" \
    --cpus "$CPUS" \
    --ram "$RAM_MB" \
    "${CPU_MODEL_ARGS[@]}"

echo -e "\n${BOLD}${GREEN}Installation session ended.${RESET}"
echo -e "To boot your installed FreeBSD VM from ${BOLD}${TARGET_DISK}${RESET} in the future:"
if [[ $USE_SUDO == true ]]; then
    echo -e "${CYAN}sudo ./target/debug/cube --freebsd ${EXTRACTED_KERNEL} --disk ${TARGET_DISK} --net vmnet --cpus ${CPUS} --multiuser --ssh-port 2222${RESET}"
else
    echo -e "${CYAN}./target/debug/cube --freebsd ${EXTRACTED_KERNEL} --disk ${TARGET_DISK} --net vmnet-proxy --cpus ${CPUS} --multiuser --ssh-port 2222${RESET}"
fi
echo -e "GPT auto-detects UFS vs ZFS. ${BOLD}--module zfs.ko${RESET} is only needed for Auto (ZFS);"
echo -e "if you chose ZFS, keep ${CYAN}boot/kernel/zfs.ko${RESET} (cached from the ISO above).\n"
echo -e "${YELLOW}[Network Troubleshooting Tip]${RESET}"
echo -e "If the installed VM has 'No route to host' or cannot resolve domain names, run inside the VM:"
echo -e "${CYAN}echo 'supersede domain-name-servers 1.1.1.1, 8.8.8.8;' >> /etc/dhclient.conf${RESET}"
echo -e "${CYAN}rm -f /etc/resolv.conf; echo 'nameserver 1.1.1.1' > /etc/resolv.conf${RESET}"
echo -e "${CYAN}echo 'nameserver 8.8.8.8' >> /etc/resolv.conf${RESET}"
echo -e "${CYAN}sysrc ifconfig_vtnet0=\"DHCP\"${RESET}"
echo -e "${CYAN}sysrc defaultrouter=\"192.168.18.1\"${RESET}"
echo -e "${CYAN}route add default 192.168.18.1${RESET}   # required now; sysrc only applies on next boot"
echo -e "${CYAN}dhclient vtnet0${RESET}\n"
