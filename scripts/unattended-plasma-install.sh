#!/usr/bin/env bash
# Regression wrapper for a FreeBSD Plasma VM. New installs should use
# scripts/install-vm.py --unattended --desktop plasma.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_DIR=$(cd "$SCRIPT_DIR/.." && pwd)
VMM_BINARY=${VMM_BINARY:-$REPO_DIR/target/debug/vmm}

if [[ ${1:-} == "--help" || ${1:-} == "-h" ]]; then
    cat <<EOF
Usage: $0 --iso FREEBSD-ARM64-DISC1.ISO [CHECK_INSTALL_OPTIONS]

Creates a fresh FreeBSD UFS disk, installs Plasma 6 with SDDM and the VMM
SCFB/input configuration, verifies it across a reboot, and retains the VM.

Useful options:
  --artifacts DIR                 Parent directory for the retained VM
  --desktop-user NAME            Login name (default: vmm)
  --desktop-password-file FILE   One-line login password file
  --root-password-file FILE      One-line root password file
  --disk-gib N                   Sparse disk size (default here: 20)
  --ram MB                       Guest RAM (default here: 4096)
  --cpus N                       Virtual CPUs (default here: 4)
  --display WIDTHxHEIGHT         Framebuffer size (default: 1280x800)
  --desktop-timeout SECONDS      Package/setup timeout (default here: 3600)
  --sudo                         In-process vmnet as root (default: vmnet-proxy)
  --skip-verify                  Write packages and Xorg configuration, then skip input and reboot checks

Set VMM_BINARY to use a different signed vmm executable.
The default needs the vmnet service (`make vmnet-service`).
EOF
    exit 0
fi

has_iso=false
for argument in "$@"; do
    [[ $argument == "--iso" || $argument == --iso=* ]] && has_iso=true
    if [[ $argument == "--desktop-environment" || $argument == --desktop-environment=* ]]; then
        echo "[Error] This wrapper always selects Plasma; omit --desktop-environment." >&2
        exit 2
    fi
done

if [[ $has_iso != true ]]; then
    echo "[Error] --iso FREEBSD-ARM64-DISC1.ISO is required." >&2
    exit 2
fi
if [[ ! -x $VMM_BINARY ]]; then
    echo "[Error] VMM binary is missing or not executable: $VMM_BINARY" >&2
    echo "Run 'make all' first, or set VMM_BINARY." >&2
    exit 2
fi

exec python3 -B "$SCRIPT_DIR/check_install.py" "$VMM_BINARY" \
    --desktop-environment plasma \
    --disk-gib 20 --ram 4096 --cpus 4 \
    --display 1280x800 --desktop-timeout 3600 \
    --keep "$@"
