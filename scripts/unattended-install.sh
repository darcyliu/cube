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
HOSTNAME="freebsd-vmm"
PASSWORD="root"
SSH_KEY=""
CPUS="2"
RAM_MB="1024"
ISO_PATH=""
BASE="distsets"
SEED_ISO="seed.iso"
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
        --base)
            BASE="$2"
            shift 2
            ;;
        --seed)
            SEED_ISO="$2"
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
            echo "      --base <MODE>      distsets (default) or pkgbase (FreeBSD 15.1+ disc1)"
            echo "      --seed <PATH>      Seed ISO path (default: seed.iso)"
            echo "      --vmm <PATH>       Cube binary (default: ./target/debug/cube)"
            echo "      --sudo             In-process vmnet as root (default: vmnet-proxy)"
            exit 0
            ;;
        *)
            echo -e "${RED}[Error] Unknown argument: $1${RESET}"
            exit 1
            ;;
    esac
done

# Step 1: Create Seed ISO
SEED_ARGS=(--output "$SEED_ISO" --hostname "$HOSTNAME" --password "$PASSWORD" --base "$BASE")
if [[ -n "$SSH_KEY" ]]; then
    SEED_ARGS+=(--ssh-key "$SSH_KEY")
fi
"$SCRIPT_DIR/create-seed.sh" "${SEED_ARGS[@]}"

# Step 2: Locate or verify ARM64 installation ISO
if [[ -z "$ISO_PATH" ]]; then
    LOCAL_ISO=$(ls FreeBSD*arm64*.iso 2>/dev/null | head -n 1 || true)
    if [[ -n "$LOCAL_ISO" && -f "$LOCAL_ISO" ]]; then
        ISO_PATH="$LOCAL_ISO"
        echo -e "${GREEN}[Cube] Auto-detected local ARM64 ISO: ${ISO_PATH}${RESET}"
    else
        DEFAULT_ISO="FreeBSD-14.2-RELEASE-arm64-aarch64-disc1.iso"
        DEFAULT_URL="https://download.freebsd.org/releases/arm64/aarch64/ISO-IMAGES/14.2/FreeBSD-14.2-RELEASE-arm64-aarch64-disc1.iso"
        echo -e "${YELLOW}[Cube] No ARM64 ISO found. Downloading FreeBSD ARM64 disc1 ISO...${RESET}"
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
echo -e "${GREEN}[Cube] Extracting kernel from ISO via bsdtar...${RESET}"
tar -xf "$ISO_PATH" -C "$EXTRACT_DIR" boot/kernel/kernel 2>/dev/null
EXTRACTED_KERNEL="${EXTRACT_DIR}/boot/kernel/kernel"

# Step 4: Create Destination Sparse Disk
if [[ ! -f "$TARGET_DISK" ]]; then
    echo -e "${GREEN}[Cube] Creating sparse destination disk '${TARGET_DISK}' (${DISK_SIZE})...${RESET}"
    truncate -s "$DISK_SIZE" "$TARGET_DISK"
else
    echo -e "${YELLOW}[Cube] Using existing destination disk '${TARGET_DISK}' (${DISK_SIZE}).${RESET}"
fi

# Step 5: Ensure Cube binary is compiled and signed
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

# Give each VM its own log and explicitly enabled UART injection mailbox.
RUN_DIR=$(mktemp -d "${TMPDIR:-/tmp}/vmm-unattended.XXXXXX")
CONSOLE_LOG="$RUN_DIR/console.log"
UART_IN="$RUN_DIR/uart-in"
FEEDER_PID=""
cleanup() {
    if [[ -n "$FEEDER_PID" ]]; then
        kill "$FEEDER_PID" 2>/dev/null || true
        wait "$FEEDER_PID" 2>/dev/null || true
    fi
    rm -f "$UART_IN" "$UART_IN.next"
}
trap cleanup EXIT

# The foreground VMM disables terminal output processing. Explicit CRLF keeps
# background diagnostics aligned, including when the guest left a partial line.
automator_log() {
    if [[ -t 1 ]]; then
        printf '\r\n%b[Automator] %s%b\r\n' "$CYAN" "$1" "$RESET"
    else
        printf '[Automator] %s\n' "$1"
    fi
}

# Publish each command atomically, so the polling reader cannot consume an
# empty/partially written file. Never overwrite a command it hasn't consumed.
send_uart() {
    local attempt
    for attempt in {1..100}; do
        [[ ! -e "$UART_IN" ]] && break
        sleep 0.1
    done
    if [[ -e "$UART_IN" ]]; then
        automator_log "UART input was not consumed; stopping automation. Log: $CONSOLE_LOG"
        return 1
    fi
    printf '%s\n' "$1" > "$UART_IN.next"
    mv "$UART_IN.next" "$UART_IN"
}

shell_ready() {
    grep -Eq '(^|[[:space:]])([^[:space:]]*root@[^[:space:]]*)?#([[:space:]]|$)' "$CONSOLE_LOG" 2>/dev/null
}

# Background console automation feeder
(
    automator_log "Monitoring FreeBSD kernel boot... Log: $CONSOLE_LOG"
    BOOTED=0
    for i in {1..35}; do
        sleep 1
        if grep -q "RETURN for /bin/sh" "$CONSOLE_LOG" 2>/dev/null; then
            automator_log "Detected single-user prompt at second ${i}!"
            BOOTED=1
            break
        fi
    done

    if [[ $BOOTED -eq 0 ]]; then
        automator_log "Single-user prompt timeout; stopping automation. Log: $CONSOLE_LOG"
        exit 1
    fi

    # Send Enter key to accept /bin/sh
    sleep 1
    send_uart ""

    # Wait for /bin/sh to finish reading .profile and for resizewin to complete
    automator_log "Waiting for root shell prompt to initialize..."
    SHELL_READY=0
    for j in {1..20}; do
        sleep 1
        if shell_ready; then
            automator_log "Single-user root shell ready at second ${j}!"
            SHELL_READY=1
            break
        fi
    done

    if [[ $SHELL_READY -eq 0 ]]; then
        automator_log "Shell prompt timeout; stopping automation. Log: $CONSOLE_LOG"
        exit 1
    fi

    # Brief delay for TTY line discipline to settle
    sleep 1

    # Send automated installation pipeline:
    # 1. Mount memory filesystems on /tmp and /var for installer scratch space
    # 2. Mount seed ISO (/dev/vtbd1) to /media
    # 3. Execute bsdinstall script non-interactively
    # 4. Power off cleanly when finished
    automator_log "Dispatching unattended bsdinstall from seed drive..."
    send_uart "mdmfs -s 64M md1 /tmp && mdmfs -s 64M md2 /var && mount -t cd9660 /dev/vtbd1 /media && bsdinstall script /media/installerconfig && echo VMM_INSTALL_COMPLETE && poweroff"
) &
FEEDER_PID=$!

# In-process vmnet installs its own PF NAT. The proxy path uses the service.
NET_MODE="vmnet-proxy"
VMM_CMD=(/usr/bin/env "VMM_UART_IN=$UART_IN" "$VMM_BIN")
if [[ $USE_SUDO == true ]]; then
    NET_MODE="vmnet"
    VMM_CMD=(sudo /usr/bin/env "VMM_UART_IN=$UART_IN" "$VMM_BIN")
    IF=$(netstat -rn -f inet 2>/dev/null | awk '/default/{print $4; exit}')
    if [[ -n "$IF" ]]; then
        sudo sysctl -w net.inet.ip.forwarding=1 >/dev/null 2>&1 || true
        printf 'nat on %s from 192.168.18.0/24 to any -> (%s)\npass in quick on bridge100 all\npass out quick on %s all\n' "$IF" "$IF" "$IF" | sudo pfctl -a "com.apple/cube" -f - 2>/dev/null || true
    fi
fi

CPU_MODEL_ARGS=()
if [[ -n "$CPU_MODEL" ]]; then
    CPU_MODEL_ARGS=(--cpu-model "$CPU_MODEL")
fi

# Launch VMM in single-user mode (omitting --multiuser) with direct serial logging
"${VMM_CMD[@]}" \
    --freebsd "$EXTRACTED_KERNEL" \
    --disk "$TARGET_DISK" \
    --disk "$SEED_ISO,ro" \
    --cdrom "$ISO_PATH" \
    --root "cd9660:/dev/vtbd2" \
    --net "$NET_MODE" \
    --cpus "$CPUS" \
    --ram "$RAM_MB" \
    --serial-log "$CONSOLE_LOG" \
    "${CPU_MODEL_ARGS[@]}"

if ! wait "$FEEDER_PID"; then
    FEEDER_PID=""
    echo "[Error] Console automation failed. Log: $CONSOLE_LOG" >&2
    exit 1
fi
FEEDER_PID=""
if ! grep -q '^VMM_INSTALL_COMPLETE' "$CONSOLE_LOG"; then
    echo "[Error] Installation did not report completion. Log: $CONSOLE_LOG" >&2
    exit 1
fi

echo -e "\n${BOLD}${GREEN}======================================================${RESET}"
echo -e "${BOLD}${GREEN}  Unattended Installation Finished!                   ${RESET}"
echo -e "${BOLD}${GREEN}======================================================${RESET}"
echo -e "Your automated FreeBSD VM is ready on ${BOLD}${TARGET_DISK}${RESET}."
echo -e "Boot your new VM with:"
if [[ $USE_SUDO == true ]]; then
    echo -e "${CYAN}sudo ./target/debug/cube --freebsd ${EXTRACTED_KERNEL} --disk ${TARGET_DISK} --net vmnet --cpus ${CPUS} --multiuser --ssh-port 2222${RESET}\n"
else
    echo -e "${CYAN}./target/debug/cube --freebsd ${EXTRACTED_KERNEL} --disk ${TARGET_DISK} --net vmnet-proxy --cpus ${CPUS} --multiuser --ssh-port 2222${RESET}\n"
fi
