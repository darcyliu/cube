# FreeBSD 14.4 ARM64 VM Runbook

This document provides the single, definitive checklist of commands for booting FreeBSD 14.4 ARM64 under the macOS Hypervisor VMM with Multi-vCPU, VirtIO-Block, and VirtIO-Net.

---

## 1. Launching the VM on macOS

From your project directory:

```bash
# Rebuild and sign with entitlements (if changes made)
cargo build
codesign -s - --entitlements entitlements.plist --force target/debug/vmm

# Launch with 2 vCPUs, 4GB disk, and vmnet NAT networking
sudo ./target/debug/vmm --freebsd boot/kernel/kernel --disk mini-memstick.img --net vmnet --cpus 2
```

> [!TIP]
> **Escape Key**: Press **`Ctrl-]`** (Control + right bracket `]`) at any time to immediately exit the VM and return to your macOS shell.

---

## 2. Boot Initialization Sequence (Inside FreeBSD)

Once at the root shell prompt (`root@:/ #`), run these steps in order:

### Step 1: Filesystem Check & Read-Write Mount
FreeBSD boots in single-user mode with `/dev/vtbd0p2` mounted read-only and marked dirty if shutdown was not clean.
```sh
fsck -y /
mount -u -w /
```

### Step 2: One-Time Disk Expansion (If not already expanded to 4GB)
```sh
sysctl kern.geom.debugflags=16
gpart recover vtbd0
gpart resize -i 2 vtbd0
growfs -y /
df -h /
```

### Step 3: Network & DNS Configuration
```sh
# 1. Obtain DHCP lease from macOS vmnet
dhclient vtnet0

# 2. Add default gateway (macOS bridge)
route add default 192.168.18.1

# 3. Configure DNS nameserver (overwrites dangling symlink)
rm -f /etc/resolv.conf
echo "nameserver 1.1.1.1" > /etc/resolv.conf

# 4. Verify system date (synchronized automatically via PL031 RTC)
date
```

---

## 3. Bootstrapping & Updating `pkg`

### Why `pkg bootstrap` Initially Hangs
The default `/usr/sbin/pkg` bootstrap utility reads `/etc/pkg/FreeBSD.conf` (which specifies `pkg+https://`) and ignores `PACKAGESITE`. Until root CA certificates (`ca_root_nss`) are installed, the TLS handshake fails to verify the FreeBSD repository certificate. Now that the VM has an emulated PL031 RTC, clock skew is eliminated, but root CA certificates are still required for HTTPS.

### The Solution: Direct Download & HTTP Repository Override
We fetch the pre-built `pkg` tarball directly via HTTP and extract it. FreeBSD packages are cryptographically signed with local keys in `/usr/share/keys/pkg`, so HTTP is completely secure. Once `ca_root_nss` is installed, HTTPS works everywhere.

```sh
# 1. Download and install pkg directly (resumable with -r -o)
fetch -r -o pkg.pkg http://pkgmir.geo.freebsd.org/FreeBSD:14:aarch64/quarterly/Latest/pkg.pkg
tar -xf pkg.pkg -C /

# 2. Configure repository to use HTTP (bypasses TLS certificate hang)
mkdir -p /usr/local/etc/pkg/repos
sed 's/pkg+https/pkg+http/' /etc/pkg/FreeBSD.conf > /usr/local/etc/pkg/repos/FreeBSD.conf

# 3. Update package catalogue
pkg update

# 4. Install root certificates so HTTPS works permanently
pkg install -y ca_root_nss
```

---

## 4. Verification
After running the above commands:
```sh
# Verify package installation works
pkg install -y curl

# Verify HTTPS works after installing ca_root_nss
curl -I https://www.freebsd.org
```

---

## 5. One-Time Setup for Automated Multi-User & SSH

To allow FreeBSD to boot unattended directly into multi-user mode (`/sbin/init`), mount filesystems automatically, start DHCP networking, and run `sshd`, execute this one-time setup inside your single-user shell:

```sh
# 1. Configure /etc/fstab for automatic read-write root mount
cat << 'EOF' > /etc/fstab
/dev/vtbd0p2  /  ufs  rw,noatime  1  1
EOF

# 2. Configure /etc/rc.conf for automated network & SSH daemon
cat << 'EOF' > /etc/rc.conf
hostname="freebsd-vmm"
ifconfig_vtnet0="DHCP"
sshd_enable="YES"
sendmail_enable="NONE"
dumpdev="NO"
EOF

# 3. Disable installer autorun (from installer memstick)
rm -f /etc/rc.local

# 4. Enable SSH root login and set a password
sed -i '' 's/#PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config
sed -i '' 's/#PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config

# Set a root password (e.g. 'root' or your choice)
passwd root

# (Optional) Authorize your host Mac's SSH public key for instant key-based login:
# mkdir -p /root/.ssh
# echo "YOUR_HOST_SSH_PUBLIC_KEY" >> /root/.ssh/authorized_keys
# chmod 700 /root/.ssh && chmod 600 /root/.ssh/authorized_keys

# 5. Flush filesystem to disk
sync
```

Press **`Ctrl-]`** to exit the VM back to macOS.

---

## 6. Multi-User Boot & SSH Access from macOS

Once provisioned, boot FreeBSD directly in multi-user mode with port forwarding:

```bash
sudo ./target/debug/vmm --freebsd boot/kernel/kernel --disk mini-memstick.img --net vmnet --cpus 2 --multiuser --ssh-port 2222
```

FreeBSD will boot `/sbin/init`, run `/etc/rc`, bring up `vtnet0` via DHCP (`192.168.18.2`), start `sshd`, and present a login prompt on `ttyu0`.

### Connecting via SSH from macOS

Open a standard macOS Terminal window and run either:

```bash
# Option A: Direct connection over macOS vmnet bridge
ssh root@192.168.18.2

# Option B: Local port-forward connection
ssh -p 2222 root@localhost
```

