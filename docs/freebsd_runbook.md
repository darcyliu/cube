# FreeBSD 14.4 ARM64 VM Runbook

This document provides the single, definitive checklist of commands for booting FreeBSD 14.4 ARM64 under the macOS Hypervisor VMM with Multi-vCPU, VirtIO-Block, and VirtIO-Net.

---

## 1. Launching the VM on macOS

From your project directory:

```bash
# Rebuild and sign with entitlements (if changes made)
cargo build
codesign -s - --entitlements entitlements.plist --force target/debug/cube

# Launch with 2 vCPUs, 4GB disk, and vmnet NAT networking
sudo ./target/debug/cube --freebsd boot/kernel/kernel --disk mini-memstick.img --net vmnet --cpus 2
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

# 2. Configure /etc/rc.conf for automated network, SSH daemon & auto-growfs
cat << 'EOF' > /etc/rc.conf
hostname="freebsd-vmm"
ifconfig_vtnet0="DHCP"
sshd_enable="YES"
growfs_enable="YES"
sendmail_enable="NONE"
dumpdev="NO"
EOF

# 3. Clean up installer symlinks and configure DNS
rm -f /etc/rc.local
rm -f /etc/resolv.conf
echo "nameserver 1.1.1.1" > /etc/resolv.conf

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
sudo ./target/debug/cube --freebsd boot/kernel/kernel --disk mini-memstick.img --net vmnet --cpus 2 --multiuser --ssh-port 2222
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

---

## 7. Multi-Disk Storage & Copy-On-Write (COW) Overlays

### Attaching Multiple Disks
The VMM supports attaching multiple block devices simultaneously. Each disk appears sequentially inside FreeBSD (`vtbd0`, `vtbd1`, `vtbd2`...):

```bash
# Example: Boot OS on vtbd0 and attach a secondary data disk on vtbd1
sudo ./target/debug/cube --freebsd boot/kernel/kernel \
    --disk mini-memstick.img \
    --disk /path/to/secondary_data.img \
    --net vmnet --cpus 2 --multiuser --ssh-port 2222
```

Inside FreeBSD, verify disks with:
```sh
geom disk list
# Format and mount secondary disk:
gpart create -s gpt vtbd1
gpart add -t freebsd-ufs -l data0 vtbd1
newfs -U /dev/vtbd1p1
mkdir -p /mnt/data && mount /dev/vtbd1p1 /mnt/data
```

### Attaching Read-Only ISOs or Cloud-Init Seed Drives
To attach an installer ISO or cloud-init config drive:
```bash
sudo ./target/debug/cube --freebsd boot/kernel/kernel \
    --disk mini-memstick.img \
    --cdrom seed.iso \
    --net vmnet --cpus 2 --multiuser
```

### Copy-On-Write (COW) Overlays (Safe Disposable Testing)
To experiment or test changes without modifying your base image, enable COW:
```bash
# Enables COW for all attached disks (creates <disk>.cow overlay file)
sudo ./target/debug/cube --freebsd boot/kernel/kernel \
    --disk mini-memstick.img \
    --cow \
    --net vmnet --cpus 2 --multiuser
```
The base image `mini-memstick.img` remains untouched (read-only). All sector modifications are written to `mini-memstick.img.cow`. Deleting the `.cow` file instantly resets the VM to its clean base state.

---

## 8. Terminal Window Resizing & Geometry Handling (SIGWINCH)

Full-screen terminal applications like `vim`, `htop`, and `tmux` require accurate terminal geometry (columns and rows) to render without clipping or visual distortion. The hypervisor provides end-to-end window resize support across all access methods:

### 1. VirtIO-Console Hardware Resize Forwarding (`virtio-console`, Device ID 3)
* **MMIO Location**: GPA `0x0a00_0600`, Size `0x200`, IRQ `38` (SPI 6).
* **How It Works**:
  - The VMM implements VirtIO Console conforming to VirtIO 1.1 with `VIRTIO_CONSOLE_F_SIZE` (bit 0).
  - A background host thread (`vmm-sigwinch`) listens for macOS `SIGWINCH` signals (signal 28) and queries the active window size using `ioctl(0, TIOCGWINSZ, &ws)`.
  - When you resize your macOS Terminal window, the host updates the VirtIO console configuration registers (`cols` and `rows`) and asserts a **VirtIO Configuration Change Interrupt** (`InterruptStatus |= 2`).
  - FreeBSD's `virtio_console` driver (`vtcon`) catches the interrupt, reads the updated geometry, updates the TTY (`tty_set_winsize()`), and sends `SIGWINCH` to foreground applications automatically.

### 2. Native SSH Session Resizing
When connected over SSH (`ssh -p 2222 root@localhost` or `ssh root@192.168.18.2`):
* Terminal resizing is completely seamless and handled natively by the SSH protocol (RFC 4254 §6.7 `window-change`).
* Every time you resize macOS Terminal, your SSH client transmits the new geometry to FreeBSD's `sshd`, which applies it to the allocated PTY with `TIOCSWINSZ`.
* Applications like `tmux`, `htop`, and `vim` adapt instantly and reflow smoothly.

### 3. Serial Console Login Synchronization (`/dev/ttyu0`)
On raw serial connections, standard RS-232 hardware lacks an out-of-band wire for window dimensions. To ensure your shell and full-screen tools on `ttyu0` immediately adopt your macOS terminal dimensions upon login, add FreeBSD's built-in `resizewin` utility to `/root/.profile`:

```sh
# Inside FreeBSD:
echo 'eval $(resizewin -z)' >> /root/.profile
```

When you log into the serial console, `resizewin` queries your terminal emulator via ANSI escape sequences and applies the exact window dimensions to the kernel TTY.

---

## 9. Automated First-Boot Expansion & Cloud VM Images

To completely eliminate manual `gpart` and `growfs` commands when reinstalling or creating fresh VMs, FreeBSD provides native automated filesystem expansion via `rc.d/growfs`.

### Method A: Automated Expansion via `growfs_enable="YES"`
If `growfs_enable="YES"` is present in `/etc/rc.conf`, FreeBSD automatically checks on every boot whether the backing virtual disk has grown beyond its partition boundary.

If you resize or create a new disk on macOS:
```bash
# Expand any raw disk to 10GB (sparse - occupies no extra physical disk space until written)
truncate -s 10g mini-memstick.img
```
On the very next boot, FreeBSD's `/etc/rc.d/growfs` startup service will automatically:
1. Detect unallocated disk blocks at the end of the disk.
2. Relocate the secondary GPT partition header.
3. Grow the root partition (`vtbd0p2`) to fill the entire 10GB.
4. Expand the UFS root filesystem live—**with zero manual commands required**.

### Method B: Deploying Official FreeBSD Cloud / VM Images
FreeBSD publishes official, pre-installed, and pre-configured virtual machine images (`VM-IMAGES`) designed for hypervisors:

1. **Download and Decompress the Official ARM64 VM Image**:
   ```bash
   # Download FreeBSD ARM64 raw VM image (or latest release)
   curl -LO https://download.freebsd.org/releases/VM-IMAGES/14.2-RELEASE/aarch64/Latest/FreeBSD-14.2-RELEASE-arm64-aarch64.raw.xz

   # Decompress raw disk image
   unxz FreeBSD-14.2-RELEASE-arm64-aarch64.raw.xz
   mv FreeBSD-14.2-RELEASE-arm64-aarch64.raw freebsd-cloud.img
   ```

2. **Resize to Desired Capacity**:
   ```bash
   # Expand to desired size (e.g. 20GB)
   truncate -s 20g freebsd-cloud.img
   ```

3. **Boot Directly in Multi-User Mode**:
   ```bash
   sudo ./target/debug/cube --freebsd boot/kernel/kernel \
       --disk freebsd-cloud.img \
       --net vmnet --cpus 2 --multiuser --ssh-port 2222
   ```

* **Zero-Touch Automation**: Official FreeBSD cloud images come with `growfs_enable="YES"`, DHCP, and SSH pre-configured. On first boot, the system expands the root disk to 20GB and presents the login prompt immediately, completely bypassing single-user mode.

---

## 10. Installing FreeBSD from an Official Installation ISO (`disc1.iso`)

You can install a fresh FreeBSD system onto a virtual hard drive from the official FreeBSD CD/DVD installer ISO (`disc1.iso` or `bootonly.iso`).

### 1. Automated Installation Tool
We provide a dedicated tool [`scripts/install-from-iso.sh`](../scripts/install-from-iso.sh) that automates the entire process:

```bash
# Run the automated installer tool (or pass an explicit ISO path)
make install-iso
# Or:
./scripts/install-from-iso.sh --iso FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso
```

**What the script does automatically**:
1. **Auto-Detects Local ARM64 ISO**: Finds any `FreeBSD-*-arm64-aarch64*.iso` in the workspace (or downloads it if missing).
2. **Architecture Safety Verification**: Uses `tar` and `file` to verify the ISO contains an `ARM aarch64` kernel, preventing crashes from incompatible architectures like RISC-V or x86.
3. **Zero-Mount Kernel Extraction**: Extracts `boot/kernel/kernel` in under 1 second via macOS `bsdtar` directly without requiring root or mounting.
4. **Target Disk Creation**: Allocates a 20GB sparse virtual disk (`freebsd-installed.img`).
5. **Boots the Installer**: Boots the VMM with:
   - Target Hard Drive: `/dev/vtbd0` (`--disk freebsd-installed.img`)
   - CD-ROM ISO: `/dev/vtbd1` (`--cdrom <iso>`)
   - Root Mount: `cd9660:/dev/vtbd1` (`--root "cd9660:/dev/vtbd1"`)

---

### 2. Why `diskutil mount` Fails on macOS & How to Extract Files
FreeBSD installer images format their filesystems as **UFS2** (Unix File System 2). Because macOS removed native UFS filesystem support over a decade ago, running `diskutil image attach` or `hdiutil attach` will fail with:
```text
Error: Failed to mount the volume
```

**The Solution**: macOS ships with `bsdtar` (`/usr/bin/tar`) backed by `libarchive`. It can read and extract files directly from ISO9660 and UFS images without mounting them:
```bash
# List files inside the ISO:
tar -tf FreeBSD-*-disc1.iso | head -n 25

# Extract the kernel directly:
tar -xf FreeBSD-*-disc1.iso boot/kernel/kernel
```

---

### 3. Architecture Requirement: `arm64-aarch64`
Our hypervisor executes on Apple Silicon using hardware virtualization (`Hypervisor.framework`).
* **Correct**: `FreeBSD-14.x-RELEASE-arm64-aarch64-disc1.iso`
* **Incompatible**: `FreeBSD-*-riscv-riscv64-*.iso` (RISC-V CPU) or `FreeBSD-*-amd64-*.iso` (Intel/AMD x86_64).

---

### 4. Completing the Interactive Installation
1. When FreeBSD boots, the standard `bsdinstall` interface appears.
2. Select **Install**.
3. When prompted for partitioning, choose **`Auto (UFS)`** on **`/dev/vtbd0`**.

> [!IMPORTANT]
> **Filesystem Selection:** **Auto (UFS)** is the default and does not need extra modules. **Auto (ZFS)** is supported when `boot/kernel/zfs.ko` is present (`make fetch-freebsd`): the VMM preloads it via FreeBSD MODINFO and sets `vfs.root.mountfrom=zfs:zroot/ROOT/default`. Architecture: [docs/freebsd_zfs_boot.md](freebsd_zfs_boot.md).

4. Complete the installation wizard (set hostname, root password, and network).
5. When finished, choose **Reboot** or press **`Ctrl-]`** to return to macOS.

---

### 5. Network & DNS Configuration (Internet Access)
Under macOS `vmnet.framework` in Shared/NAT mode:
* macOS's built-in DHCP server (`bootpd`) automatically allocates an IP address (e.g., `192.168.18.2`) and default gateway (`192.168.18.1`).
* **Crucial Note on DNS:** macOS does **not** run a local DNS resolver on `192.168.18.1:53`. To enable internet access (domain name resolution for `pkg`, `ping google.com`, `curl`, etc.), FreeBSD must be configured to use public DNS servers (e.g., `1.1.1.1` and `8.8.8.8`) and instruct `dhclient` not to overwrite them with the host's IP:

Run inside the FreeBSD guest:
```bash
# 1. Instruct dhclient to always use public DNS servers
cat << 'EOF' > /etc/dhclient.conf
supersede domain-name-servers 1.1.1.1, 8.8.8.8;
EOF

# 2. Write nameservers to /etc/resolv.conf
cat << 'EOF' > /etc/resolv.conf
nameserver 1.1.1.1
nameserver 8.8.8.8
EOF

# 3. Ensure DHCP is enabled in /etc/rc.conf
sysrc ifconfig_vtnet0="DHCP"

# 4. Refresh network interface
service netif restart vtnet0
dhclient vtnet0
```

---

### 6. Booting the Newly Installed System
Once installation is finished, boot directly from your newly installed virtual drive:
```bash
sudo ./target/debug/cube --freebsd .iso-boot/boot/kernel/kernel \
    --disk freebsd-installed.img \
    --net vmnet --cpus 2 --multiuser --ssh-port 2222
```

---

## 11. Automated 100% Unattended "Seed" Installation (`make install-seed`)

To achieve a fully automated, hands-off installation without answering any prompts, you can attach an automated **Seed Drive** (`CIDATA`) containing FreeBSD's `installerconfig` and Cloud-Init metadata.

### 1. The Seed Generator Tool (`scripts/create-seed.sh`)
We provide [`scripts/create-seed.sh`](../scripts/create-seed.sh) which builds a standard `CIDATA` seed ISO directly on macOS using user-space tools (zero root or mounting required):

```bash
# Generate seed.iso with default password 'root' and auto-detected SSH key
./scripts/create-seed.sh

# Or customize hostname, password, and SSH public key:
./scripts/create-seed.sh --hostname freebsd-node-1 --password "mysecretpass" --ssh-key ~/.ssh/id_ed25519.pub
```

The generated `seed.iso` contains:
* `installerconfig`: Automates disk partitioning on `/dev/vtbd0`, base and kernel extraction, DHCP configuration, SSH root login, and `growfs_enable="YES"`.
* `meta-data`: Cloud-Init instance ID and hostname.
* `user-data`: Cloud-Init user credentials and authorized SSH keys.

---

### 2. One-Command Unattended Installation (`make install-seed`)
Run the all-in-one automated installer tool:

```bash
make install-seed
# Or:
./scripts/unattended-install.sh --password "secret" --size 30g --cpus 4
```

**What happens automatically**:
1. Builds `seed.iso` with your configured credentials and SSH keys.
2. Auto-detects your local ARM64 FreeBSD installer ISO.
3. Allocates a 20GB sparse disk (`freebsd-unattended.img`).
4. Boots the VMM with:
   - Destination Disk: `/dev/vtbd0` (`--disk freebsd-unattended.img`)
   - Seed Drive: `/dev/vtbd1` (`--disk seed.iso,ro`)
   - Installer Media: `/dev/vtbd2` (`--cdrom <iso>`)
5. FreeBSD executes `installerconfig` non-interactively, provisions `/dev/vtbd0`, installs all packages, sets passwords, and shuts down—**with zero manual input required**.

---

### 3. Booting Your Unattended VM
Boot directly from your freshly provisioned disk:
```bash
sudo ./target/debug/cube --freebsd .iso-boot/boot/kernel/kernel \
    --disk freebsd-unattended.img \
    --net vmnet --cpus 2 --multiuser --ssh-port 2222
```
Connect immediately from macOS:
```bash
ssh root@localhost -p 2222
```

---

## 12. Workspace & Instance Cleanup Tool (`make clean-vm`)

To reset your workspace and test installations from scratch without manually removing multi-gigabyte disk images or killing stale processes, use [`scripts/clean-vms.sh`](../scripts/clean-vms.sh):

```bash
# Interactive cleanup (prompts for confirmation and lists files)
make clean-vm
# Or:
./scripts/clean-vms.sh

# Non-interactive quick reset:
./scripts/clean-vms.sh -y

# Complete reset (including wiping downloaded installer ISOs):
./scripts/clean-vms.sh --all
```

**What it does:**
1. **Detects & Stops Stale VMMs:** Automatically finds any active `vmm` hypervisor processes holding disk image locks and terminates them cleanly.
2. **Deletes Virtual Disks:** Cleans `freebsd-installed.img`, `freebsd-unattended.img`, and any `.cow` or `.overlay` files.
3. **Cleans Seed & Kernel Trees:** Removes `seed.iso`, `.seed-staging/`, and `.iso-boot/`.
4. **Flushes Host IPC & Logs:** Clears `/tmp/vmm-uart-in`, `/tmp/vmm-*-console.log`, and network debug files.
5. **Preserves Installer ISOs:** Keeps downloaded `FreeBSD*.iso` images intact by default so you do not have to wait for large downloads when retesting.

---

## 13. CPU Model Identification & Spoofing (`--cpu-model`)

By default on ARM64 FreeBSD, the kernel identifies processors during boot by reading the **Main ID Register (`MIDR_EL1`)** via `sys/arm64/arm64/identcpu.c`.

### Why it previously showed "Apple Unknown CPU"
Under Apple's `Hypervisor.framework`, the default virtualized `MIDR_EL1` (internally mapped to `VPIDR_EL2`) is `0x610f0000`:
* **Implementer (bits 31:24):** `0x61` ('a' = Apple)
* **Variant (bits 23:20):** `0x0`
* **Architecture (bits 19:16):** `0xf` (ARMv8 CPUID)
* **Part Number (bits 15:4):** `0x000` (**Unknown**)
* **Revision (bits 3:0):** `0x0` (`r0p0`)

Because part number `0x000` does not match any recognized core in FreeBSD's table, FreeBSD fell back to:
```text
CPU: Apple Unknown r0p0 (midr: 610f0000) (2)
```

### Automatic Host CPU Detection (Default)
The VMM now queries macOS `machdep.cpu.brand_string` on startup without any external dependencies and automatically programs `HV_SYS_REG_MIDR_EL1` across all boot and secondary vCPUs.
* On an M1 Max Mac: automatically reports **`Apple M1 Max Firestorm`** (`0x610f0290`).
* On an M2 Mac: automatically reports **`Apple M2 Avalanche`** (`0x610f0330`).
* Inside FreeBSD, `dmesg` and `sysctl hw.model` will now display:
```text
CPU  0: Apple M1 Max Firestorm r0p0 affinity:  0
CPU  1: Apple M1 Max Firestorm r0p0 affinity:  1
```

### Customizing / Spoofing the CPU Model
You can override the CPU identity using the `--cpu-model` CLI option:

| CLI Option | Target Processor | MIDR Value | FreeBSD Output |
| :--- | :--- | :--- | :--- |
| `--cpu-model auto` | Host Apple Silicon (default) | Auto | Host Chip (`Apple M1 Max Firestorm`, etc.) |
| `--cpu-model m2` | Apple M2 Avalanche (P-core) | `0x610f0330` | `Apple M2 Avalanche r0p0` |
| `--cpu-model m2-max` | Apple M2 Max Avalanche | `0x610f0390` | `Apple M2 Max Avalanche r0p0` |
| `--cpu-model m1` | Apple M1 Firestorm (P-core) | `0x610f0230` | `Apple M1 Firestorm r0p0` |
| `--cpu-model m1-max` | Apple M1 Max Firestorm | `0x610f0290` | `Apple M1 Max Firestorm r0p0` |
| `--cpu-model neoverse-n2` | ARM Neoverse-N2 (Server) | `0x410fd490` | `ARM Neoverse-N2 r0p0` |
| `--cpu-model cortex-a78` | ARM Cortex-A78 | `0x410fd410` | `ARM Cortex-A78 r0p0` |
| `--cpu-model 0x<hex>` | Any custom MIDR value | User Hex | Custom parsed CPU identity |

#### Example Commands:
```bash
# Boot with ARM Neoverse-N2 server identity:
sudo ./target/debug/cube --freebsd boot/kernel/kernel --disk freebsd-installed.img --cpus 2 --cpu-model neoverse-n2

# Install with Apple M2 Avalanche identity:
./scripts/install-from-iso.sh --cpu-model m2
```







