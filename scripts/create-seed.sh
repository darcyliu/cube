#!/usr/bin/env bash
# ==============================================================================
# scripts/create-seed.sh
#
# Generates a Cloud-Init / BSDInstall Seed ISO (CIDATA) on macOS
# Uses standard user-space 'hdiutil makehybrid' (no root or mounting required).
# ==============================================================================

set -e

OUTPUT_ISO="seed.iso"
HOSTNAME="freebsd-vmm"
PASSWORD="root"
SSH_PUB_KEY=""
SEED_DIR=".seed-staging"
FILESYSTEM="ufs"

# ANSI colors
BOLD="\033[1m"
GREEN="\033[1;32m"
CYAN="\033[1;36m"
RESET="\033[0m"

# Parse CLI arguments
while [[ $# -gt 0 ]]; do
    case "$1" in
        --filesystem)
            FILESYSTEM="$2"
            shift 2
            ;;
        --output|-o)
            OUTPUT_ISO="$2"
            shift 2
            ;;
        --hostname|-h)
            HOSTNAME="$2"
            shift 2
            ;;
        --password|-p)
            PASSWORD="$2"
            shift 2
            ;;
        --ssh-key|-k)
            if [[ -f "$2" ]]; then
                SSH_PUB_KEY=$(cat "$2")
            else
                SSH_PUB_KEY="$2"
            fi
            shift 2
            ;;
        *)
            echo "Usage: $0 [OPTIONS]"
            echo "Options:"
            echo "  -o, --output <FILE>     Output ISO file (default: seed.iso)"
            echo "  -h, --hostname <NAME>   VM Hostname (default: freebsd-vmm)"
            echo "  -p, --password <PASS>   Root password (default: root)"
            echo "  -k, --ssh-key <KEY|FILE> SSH public key or key file to authorize"
            echo "      --filesystem <FS>   ufs (default) or zfs with an EFI system partition"
            exit 0
            ;;
    esac
done

case "$FILESYSTEM" in
    ufs|zfs) ;;
    *) echo "Invalid filesystem: $FILESYSTEM" >&2; exit 1 ;;
esac

# If no SSH key was specified, check default ~/.ssh/id_ed25519.pub or id_rsa.pub
if [[ -z "$SSH_PUB_KEY" ]]; then
    if [[ -r "$HOME/.ssh/id_ed25519.pub" ]]; then
        SSH_PUB_KEY=$(cat "$HOME/.ssh/id_ed25519.pub" 2>/dev/null || true)
    elif [[ -r "$HOME/.ssh/id_rsa.pub" ]]; then
        SSH_PUB_KEY=$(cat "$HOME/.ssh/id_rsa.pub" 2>/dev/null || true)
    fi
fi

echo -e "${BOLD}${CYAN}[VMM] Generating Seed ISO '${OUTPUT_ISO}' (Volume: CIDATA)...${RESET}"

rm -rf "$SEED_DIR" "$OUTPUT_ISO"
mkdir -p "$SEED_DIR"

# 1. Generate meta-data (Standard Cloud-Init NoCloud format)
cat << EOF > "$SEED_DIR/meta-data"
instance-id: ${HOSTNAME}-01
local-hostname: ${HOSTNAME}
EOF

# 2. Generate user-data (Cloud-Init format)
cat << EOF > "$SEED_DIR/user-data"
#cloud-config
hostname: ${HOSTNAME}
password: ${PASSWORD}
chpasswd: { expire: False }
ssh_pwauth: True
EOF

if [[ -n "$SSH_PUB_KEY" ]]; then
cat << EOF >> "$SEED_DIR/user-data"
ssh_authorized_keys:
  - "${SSH_PUB_KEY}"
EOF
fi

# 3. Generate installerconfig (FreeBSD bsdinstall automated script)
cat << EOF > "$SEED_DIR/installerconfig"
PARTITIONS="vtbd0"
DISTRIBUTIONS="kernel.txz base.txz"
export nonInteractive="YES"

#!/bin/sh
cat << 'RC' > /etc/rc.conf
hostname="${HOSTNAME}"
ifconfig_vtnet0="DHCP"
defaultrouter="192.168.18.1"
sshd_enable="YES"
growfs_enable="YES"
sendmail_enable="NONE"
dumpdev="NO"
RC

echo "${PASSWORD}" | pw usermod root -h 0
sed -i '' 's/#PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config
sed -i '' 's/#PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config
cat << 'DHCP' > /etc/dhclient.conf
supersede domain-name-servers 1.1.1.1, 8.8.8.8;
DHCP
echo "nameserver 1.1.1.1" > /etc/resolv.conf
echo "nameserver 8.8.8.8" >> /etc/resolv.conf
EOF

if [[ -n "$SSH_PUB_KEY" ]]; then
cat << EOF >> "$SEED_DIR/installerconfig"
mkdir -p /root/.ssh
echo "${SSH_PUB_KEY}" >> /root/.ssh/authorized_keys
chmod 700 /root/.ssh && chmod 600 /root/.ssh/authorized_keys
EOF
fi

if [[ "$FILESYSTEM" == "zfs" ]]; then
    # These exported settings are consumed by bsdinstall's zfsboot subprocess.
    sed -i '' '/^PARTITIONS=/c\
export ZFSBOOT_DISKS="vtbd0"\
export ZFSBOOT_VDEV_TYPE="stripe"\
export ZFSBOOT_POOL_NAME="zroot"\
export ZFSBOOT_PARTITION_SCHEME="GPT"\
export ZFSBOOT_BOOT_TYPE="UEFI"\
export ZFSBOOT_SWAP_SIZE="1g"\
export ZFSBOOT_CONFIRM_LAYOUT=""\
' "$SEED_DIR/installerconfig"
    cat << 'EOF' >> "$SEED_DIR/installerconfig"
sysrc zfs_enable="YES"
cat << 'LOADER' >> /boot/loader.conf
zfs_load="YES"
boot_serial="YES"
autoboot_delay="1"
LOADER
EOF
fi

# Build ISO with volume name CIDATA
hdiutil makehybrid -o "$OUTPUT_ISO" -iso -joliet -default-volume-name CIDATA "$SEED_DIR" >/dev/null 2>&1

rm -rf "$SEED_DIR"

echo -e "${GREEN}[VMM] Seed ISO created successfully: ${OUTPUT_ISO}${RESET}"
echo -e "  Volume Label : CIDATA"
echo -e "  Hostname     : ${HOSTNAME}"
echo -e "  Root Password: ${PASSWORD}"
if [[ -n "$SSH_PUB_KEY" ]]; then
    echo -e "  SSH Key      : Included (authorized_keys)"
fi
