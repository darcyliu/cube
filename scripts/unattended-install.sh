#!/usr/bin/env bash
# ==============================================================================
# scripts/unattended-install.sh
#
# Automated 100% Unattended "Seed" Installation for FreeBSD on macOS ARM64 VMM
# Uses a Cloud-Init / BSDInstall CIDATA Seed Drive
# ==============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_DISK="freebsd-unattended.img"
DISK_SIZE="20g"
SEED_ISO="seed.iso"
HOSTNAME="freebsd-vmm"
PASSWORD="root"
SSH_KEY=""
CPUS="2"
RAM_MB="1024"
ISO_PATH=""

# ANSI color codes
BOLD="\033[1m"
GREEN="\033[1;32m"
YELLOW="\033[1;33m"
RED="\033[1;31m"
CYAN="\033[1;36m"
RESET="\033[0m"

echo -e "${BOLD}${CYAN}======================================================${RESET}"
echo -e "${BOLD}${CYAN}  macOS Hypervisor: Automated Seed Installation       ${RESET}"
echo -e "${BOLD}${CYAN}======================================================${RESET}\n"

# Parse CLI arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --disk|-d)
            TARGET_DISK="$2"
            shift 2
            ;;
        --size|-s)
            DISK_SIZE="$2"
            shift 2
            ;;
        --iso|-i)
            ISO_PATH="$2"
            shift 2
            ;;
        --password|-p)
            PASSWORD="$2"
            shift 2
            ;;
        --hostname|-h)
            HOSTNAME="$2"
            shift 2
            ;;
        --ssh-key|-k)
            SSH_KEY="$2"
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
        --help)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  -d, --disk <PATH>      Destination virtual disk path (default: freebsd-unattended.img)"
            echo "  -s, --size <SIZE>      Target disk size (default: 20g, sparse)"
            echo "  -i, --iso <PATH>       Path to FreeBSD ARM64 ISO image (disc1.iso)"
            echo "  -p, --password <PASS>  Root password for the installed VM (default: root)"
            echo "  -h, --hostname <NAME>  Hostname for the installed VM (default: freebsd-vmm)"
            echo "  -k, --ssh-key <KEY>    SSH public key or key file to authorize"
            echo "  -c, --cpus <N>         Number of virtual CPUs (default: 2)"
            echo "  -m, --ram <MB>         Physical RAM in MB (default: 1024)"
            echo "      --cpu-model <MOD>  CPU model/MIDR spoofing (e.g. host, m2, neoverse-n2, 0x<hex>)"
            exit 0
            ;;
        *)
            echo -e "${RED}[Error] Unknown argument: $1${RESET}"
            exit 1
            ;;
    esac
done

# Step 1: Create Seed ISO
SEED_ARGS=(--output "$SEED_ISO" --hostname "$HOSTNAME" --password "$PASSWORD")
if [[ -n "$SSH_KEY" ]]; then
    SEED_ARGS+=(--ssh-key "$SSH_KEY")
fi
"$SCRIPT_DIR/create-seed.sh" "${SEED_ARGS[@]}"

# Step 2: Locate or verify ARM64 installation ISO
if [[ -z "$ISO_PATH" ]]; then
    LOCAL_ISO=$(ls FreeBSD*arm64*.iso 2>/dev/null | head -n 1 || true)
    if [[ -n "$LOCAL_ISO" && -f "$LOCAL_ISO" ]]; then
        ISO_PATH="$LOCAL_ISO"
        echo -e "${GREEN}[VMM] Auto-detected local ARM64 ISO: ${ISO_PATH}${RESET}"
    else
        DEFAULT_ISO="FreeBSD-14.2-RELEASE-arm64-aarch64-disc1.iso"
        DEFAULT_URL="https://download.freebsd.org/releases/arm64/aarch64/ISO-IMAGES/14.2/FreeBSD-14.2-RELEASE-arm64-aarch64-disc1.iso"
        echo -e "${YELLOW}[VMM] No ARM64 ISO found. Downloading FreeBSD ARM64 disc1 ISO...${RESET}"
        curl -fSL --progress-bar -o "$DEFAULT_ISO" "$DEFAULT_URL"
        ISO_PATH="$DEFAULT_ISO"
    fi
fi

if [[ ! -f "$ISO_PATH" ]]; then
    echo -e "${RED}[Error] ISO file not found: ${ISO_PATH}${RESET}"
    exit 1
fi

# Step 3: Extract ARM64 Kernel safely via bsdtar (zero-mount)
EXTRACT_DIR=".iso-boot"
mkdir -p "$EXTRACT_DIR"
echo -e "${GREEN}[VMM] Extracting kernel from ISO via bsdtar...${RESET}"
tar -xf "$ISO_PATH" -C "$EXTRACT_DIR" boot/kernel/kernel 2>/dev/null
EXTRACTED_KERNEL="${EXTRACT_DIR}/boot/kernel/kernel"

# Step 4: Create Destination Sparse Disk
if [[ ! -f "$TARGET_DISK" ]]; then
    echo -e "${GREEN}[VMM] Creating sparse destination disk '${TARGET_DISK}' (${DISK_SIZE})...${RESET}"
    truncate -s "$DISK_SIZE" "$TARGET_DISK"
else
    echo -e "${YELLOW}[VMM] Using existing destination disk '${TARGET_DISK}' (${DISK_SIZE}).${RESET}"
fi

# Step 5: Ensure VMM binary is compiled and signed
if [[ ! -f "target/debug/vmm" ]]; then
    echo -e "${GREEN}[VMM] Building VMM binary...${RESET}"
    cargo build
    codesign -s - --entitlements entitlements.plist --force target/debug/vmm
fi

# Step 6: Launch VMM and automate installation via single-user script mode
echo -e "\n${BOLD}${GREEN}======================================================${RESET}"
echo -e "${BOLD}${GREEN}  Starting Automated Unattended Seed Installation     ${RESET}"
echo -e "${BOLD}${GREEN}======================================================${RESET}"
echo -e "  Target Disk  : ${BOLD}${TARGET_DISK}${RESET} (/dev/vtbd0)"
echo -e "  Seed Drive   : ${BOLD}${SEED_ISO}${RESET} (/dev/vtbd1, Volume: CIDATA)"
echo -e "  Installer CD : ${BOLD}${ISO_PATH}${RESET} (/dev/vtbd2)"
echo -e "  VM Hostname  : ${BOLD}${HOSTNAME}${RESET}"
echo -e "  Root Password: ${BOLD}${PASSWORD}${RESET}"
echo -e "------------------------------------------------------\n"

# Console log file for boot tracking
CONSOLE_LOG="/tmp/vmm-unattended-console.log"
rm -f "$CONSOLE_LOG" /tmp/vmm-uart-in

# Background console automation feeder
(
    echo -e "${CYAN}[Automator] Monitoring FreeBSD kernel boot...${RESET}"
    BOOTED=0
    for i in {1..35}; do
        sleep 1
        if grep -q "RETURN for /bin/sh" "$CONSOLE_LOG" 2>/dev/null; then
            echo -e "${GREEN}[Automator] Detected single-user prompt at second ${i}!${RESET}"
            BOOTED=1
            break
        fi
    done

    # Fallback if log buffering delayed the pattern
    if [[ $BOOTED -eq 0 ]]; then
        echo -e "${YELLOW}[Automator] Kernel boot interval elapsed; engaging shell...${RESET}"
    fi

    # Send Enter key to accept /bin/sh
    sleep 1
    printf "\n" > /tmp/vmm-uart-in 2>/dev/null || true

    # Wait for /bin/sh to finish reading .profile and for resizewin to complete
    echo -e "${CYAN}[Automator] Waiting for root shell prompt to initialize...${RESET}"
    SHELL_READY=0
    for j in {1..20}; do
        sleep 1
        if grep -q "root@.*#" "$CONSOLE_LOG" 2>/dev/null || grep -q "root@:/" "$CONSOLE_LOG" 2>/dev/null; then
            echo -e "${GREEN}[Automator] Single-user root shell ready at second ${j}!${RESET}"
            SHELL_READY=1
            break
        fi
    done

    if [[ $SHELL_READY -eq 0 ]]; then
        echo -e "${YELLOW}[Automator] Shell prompt detection timeout; proceeding with caution...${RESET}"
    fi

    # Brief delay for TTY line discipline to settle
    sleep 1

    # Send automated installation pipeline:
    # 1. Mount memory filesystems on /tmp and /var for installer scratch space
    # 2. Mount seed ISO (/dev/vtbd1) to /media
    # 3. Execute bsdinstall script non-interactively
    # 4. Power off cleanly when finished
    echo -e "${GREEN}[Automator] Dispatching unattended bsdinstall from seed drive...${RESET}"
    printf "mdmfs -s 64M md1 /tmp && mdmfs -s 64M md2 /var && mount -t cd9660 /dev/vtbd1 /media && bsdinstall script /media/installerconfig && poweroff\n" > /tmp/vmm-uart-in 2>/dev/null || true
) &
FEEDER_PID=$!

# Configure macOS PF NAT forwarding for guest internet access
IF=$(netstat -rn -f inet 2>/dev/null | awk '/default/{print $4; exit}')
if [[ -n "$IF" ]]; then
    sudo sysctl -w net.inet.ip.forwarding=1 >/dev/null 2>&1 || true
    printf 'nat on %s from 192.168.18.0/24 to any -> (%s)\npass in quick on bridge100 all\npass out quick on %s all\n' "$IF" "$IF" "$IF" | sudo pfctl -a "com.apple/vmm" -f - 2>/dev/null || true
fi

CPU_MODEL_ARGS=()
if [[ -n "$CPU_MODEL" ]]; then
    CPU_MODEL_ARGS=(--cpu-model "$CPU_MODEL")
fi

# Launch VMM in single-user mode (omitting --multiuser) with direct serial logging
sudo ./target/debug/vmm \
    --freebsd "$EXTRACTED_KERNEL" \
    --disk "$TARGET_DISK" \
    --disk "$SEED_ISO,ro" \
    --cdrom "$ISO_PATH" \
    --root "cd9660:/dev/vtbd2" \
    --net vmnet \
    --cpus "$CPUS" \
    --ram "$RAM_MB" \
    --serial-log "$CONSOLE_LOG" \
    "${CPU_MODEL_ARGS[@]}"

wait $FEEDER_PID 2>/dev/null || true
rm -f "$CONSOLE_LOG" /tmp/vmm-uart-in

echo -e "\n${BOLD}${GREEN}======================================================${RESET}"
echo -e "${BOLD}${GREEN}  Unattended Installation Finished!                   ${RESET}"
echo -e "${BOLD}${GREEN}======================================================${RESET}"
echo -e "Your automated FreeBSD VM is ready on ${BOLD}${TARGET_DISK}${RESET}."
echo -e "Boot your new VM with:"
echo -e "${CYAN}sudo ./target/debug/vmm --freebsd ${EXTRACTED_KERNEL} --disk ${TARGET_DISK} --net vmnet --cpus ${CPUS} --multiuser --ssh-port 2222${RESET}\n"
