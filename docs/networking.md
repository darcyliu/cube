# VirtIO-Net & host networking

## Real NIC quick start (vmnet)

Default `--net vmnet` tries macOS **vmnet.framework** (shared/NAT). Without
root or the restricted `com.apple.vm.networking` entitlement it fails and the
VMM exits with an error. Failed vmnet, bridge, and TAP initialization never
silently selects mock networking. Use `--net mock` explicitly to discard packets,
or `--net none` to omit the NIC.

**Working path today:**

```sh
make all
# UEFI + ZFS disk with a real NIC:
sudo make sudo-freebsd-zfs-uefi DISK=freebsd-installed.img
```

Inside FreeBSD (multi-user usually gets DHCP automatically):

```sh
ifconfig vtnet0
# if needed:
dhclient vtnet0
echo "nameserver 1.1.1.1" > /etc/resolv.conf
ping -c 2 1.1.1.1
```

Guest traffic uses the vmnet shared subnet (documented below as
`192.168.18.0/24`). The `sudo-*` Makefile targets also install a PF NAT anchor
so that subnet can reach the internet.

**Bridged (LAN DHCP, no host PF NAT):**
```sh
sudo ./target/debug/vmm ... --net bridge        # default outbound iface (route get default)
sudo ./target/debug/vmm ... --net bridge:en0    # explicit iface
```
Guest gets an address from your LAN router. Still needs `sudo` (or `com.apple.vm.networking`) to *create* the vmnet interface — bridge only removes the PF NAT step.

**Force mock** (no sudo, no external routing): `--net mock`  
**Disable NIC:** `--net none`

Codesigning details: [entitlements.md](entitlements.md).

## TCP forwarding

Use a repeatable rule with numeric host and guest addresses:

```sh
sudo ./target/debug/vmm --freebsd boot/kernel/kernel --disk freebsd-installed.img \
  --multiuser --net vmnet --forward 127.0.0.1:2222=192.168.18.7:22 \
  --forward 127.0.0.1:8080=192.168.18.7:80
ssh -p 2222 root@127.0.0.1
```

Choose the guest address from its actual DHCP lease or static configuration;
forwarding does not discover it. IPv6 endpoints use brackets, for example
`--forward '[::1]:2222=[fd00::7]:22'`. Both ports must be nonzero.
`--ssh-port 2222` forwards from `127.0.0.1:2222` to port 22 at the IPv4 address
learned from a DHCP ACK for this VM's MAC address. It requires vmnet or bridge
networking and guest DHCP. Before an ACK arrives, connections fail with an
explicit diagnostic rather than guessing an address. Subsequent connections
use any newly learned address; existing connections keep their original target.
For static guest addresses, use `--forward` instead. For example, a guest at
`192.168.2.2` needs `--forward 127.0.0.1:2222=192.168.2.2:22`.
An explicit non-loopback host address exposes the listener on that interface.

An occupied or unavailable host bind address fails startup. A destination
connection failure reports the rule and OS error; retry the client after the
guest service is ready. Each rule allows 64 active connections, uses bounded
buffers, and closes its listener and connections on VM shutdown. These are TCP
forwards through the host's routing table, not UDP forwarding or a NAT backend.

Regression checks:

```sh
cargo test
python3 scripts/check_network.py target/debug/vmm
# Choose an unused address on your actual vmnet subnet for the guest check:
sudo python3 scripts/check_network.py target/debug/vmm \
  --kernel .iso-boot/boot/kernel/kernel \
  --iso FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso \
  --guest-cidr 192.168.18.77/24 --host-ip 192.168.18.1
```

The guest check uses a temporary SSH key and RAM-backed guest scratch files,
without a writable disk. It checks host connectivity and forwards SSH to guest
port 22222. It starts vmnet and uses the VMM's existing host PF/NAT setup, so
sudo is required. The basic CLI checks and TCP unit tests require permission to
open local sockets, but do not require sudo.

---

This document describes the modern VirtIO Network (virtio-net) device implementation in `vmm`, its integration with macOS `vmnet.framework`, the root causes of common single-user live media networking issues, and strategies to make the user experience (UX) seamless.

---

## 1. Network Architecture

The network subsystem provides full-duplex Ethernet connectivity between the ARM64 guest (FreeBSD 14.4) and the macOS host using Apple Silicon's hardware virtualization capabilities and standard VirtIO MMIO v2 protocols.

```
+-------------------------------------------------------------------------+
| FreeBSD Guest (Kernel & Userland)                                      |
|                                                                         |
|  fetch / curl / ssh / pkg                                               |
|      |                                                                  |
|      v (TCP / UDP / ICMP)                                               |
|  vtnet0 interface (192.168.18.2, MAC 52:54:00:12:34:56)                 |
|  Default Gateway: 192.168.18.1                                          |
+------------------------------------+------------------------------------+
                                     |
                         VirtIO MMIO v2 (GPA: 0x0a000200, IRQ 35)
                         - RX Queue 0 (128 Descriptors)
                         - TX Queue 1 (128 Descriptors)
                         - Modern Header Size: 12 bytes
                                     |
+------------------------------------v------------------------------------+
| VMM (src/devices/virtio/net.rs)                                         |
|                                                                         |
|  - Process TX Virtqueue -> vmnet_write()                                |
|  - Background RX Thread -> vmnet_read() -> Deliver RX Virtqueue         |
|  - GICv3 SPI 35 Assertion & vCPU Kick (hv_vcpus_exit)                   |
+------------------------------------+------------------------------------+
                                     |
+------------------------------------v------------------------------------+
| macOS Host Networking                                                   |
|                                                                         |
|  vmnet.framework (VMNET_SHARED_MODE) -> bridge100 (192.168.18.1)        |
|  macOS bootpd (Built-in DHCP server)                                    |
|                                                                         |
|  macOS Packet Filter (pf) NAT Anchor (com.apple/vmm):                   |
|  nat on en0 from 192.168.18.0/24 to any -> (en0)                        |
|                                                                         |
|  Physical Upstream Interface: en0 (Wi-Fi / Ethernet, 192.168.1.97)     |
+------------------------------------+------------------------------------+
                                     |
                                     v Internet (1.1.1.1, freebsd.org)
```

---

## 2. Issues Encountered & Root Cause Analysis

During initial testing of FreeBSD 14.4 single-user boot with networking, four distinct issues prevented smooth internet connectivity:

### Root Cause 1: Single-User Mode Read-Only Filesystem
* **Symptom**: `dhclient vtnet0` reported `Read-only file system` for `/var/db/dhclient.leases.vtnet0` and failed to write pidfiles.
* **Explanation**: By design, FreeBSD single-user mode (`/bin/sh`) boots with the root filesystem (`/dev/vtbd0p2`) mounted as **read-only** (`ro`).
* **Resolution**: Remount the root filesystem read-write:
  ```sh
  mount -u -w /
  # or if uncleanly shutdown:
  mount -f -u -w /
  ```

### Root Cause 2: Broken `/etc/resolv.conf` Symlink on Installer Media
* **Symptom**: `echo "nameserver 1.1.1.1" > /etc/resolv.conf` returned `-sh: cannot create /etc/resolv.conf: No such file or directory`.
* **Explanation**: On FreeBSD live/installer images (`mini-memstick.img`), `/etc/resolv.conf` is a dangling symlink pointing to `/tmp/bsdinstall_etc/resolv.conf` (or `/var/run/resolv.conf`). Because that subdirectory does not exist, standard shell redirection fails.
* **Resolution**: Remove the broken link before writing the file:
  ```sh
  rm -f /etc/resolv.conf
  echo "nameserver 1.1.1.1" > /etc/resolv.conf
  ```

### Root Cause 3: Missing Default Gateway in Kernel Routing Table
* **Symptom**: `ping: sendto: No route to host` or `fetch: transfer timed out`.
* **Explanation**: When `dhclient` failed its lease file writes on the read-only filesystem, `/sbin/dhclient-script` exited early without adding the default route (`route add default 192.168.18.1`) into FreeBSD's routing table.
* **Resolution**: Manually add the default route or re-run `dhclient` once the filesystem is read-write:
  ```sh
  route add default 192.168.18.1
  ```

### Root Cause 4: macOS Packet Filter (PF) NAT & Forwarding Drop
* **Symptom**: Guest could ping host bridge (`192.168.18.1`) and host IP (`192.168.1.97`), but pings to the home router (`192.168.1.254`) and internet IPs (`1.1.1.1`) timed out.
* **Explanation**: While Apple's `vmnet.framework` in `VMNET_SHARED_MODE` creates `bridge100` and serves DHCP, modern macOS (macOS 14+ and Sequoia) relies on macOS's Packet Filter (`pf`) to translate and forward packets between `bridge100` and physical interfaces (`en0`). By default, PF has no active NAT rule mapping `192.168.18.0/24` out of `en0`. Outbound packets were sent with un-routable private IP source `192.168.18.2` (dropped by upstream routers) or blocked by PF's forward filtering.
* **Resolution**: Insert NAT translation and pass rules into the active `com.apple/vmm` PF anchor on macOS:
  ```sh
  printf 'nat on en0 from 192.168.18.0/24 to any -> (en0)\npass in quick on bridge100 all\npass out quick on en0 all\n' | sudo pfctl -a "com.apple/vmm" -f -
  sudo sysctl -w net.inet.ip.forwarding=1
  ```

---

## 3. Quick-Start Verification Checklist

Once in the FreeBSD shell, run the following commands for guaranteed internet access:

```sh
# 1. Mount root read-write
mount -u -w /
mkdir -p /var/db /var/run

# 2. Acquire DHCP lease & configure default route
dhclient vtnet0
route add default 192.168.18.1

# 3. Configure DNS
rm -f /etc/resolv.conf
echo "nameserver 1.1.1.1" > /etc/resolv.conf
echo "nameserver 8.8.8.8" >> /etc/resolv.conf

# 4. Verify HTTP/HTTPS connectivity
fetch -o - http://example.com | head -n 15
fetch --no-verify-peer -o - https://www.freebsd.org | head -n 15
```

---

## 4. UX Improvements: Making Networking 100% Seamless

To avoid manual `pfctl` commands, shell mounts, and manual route additions in future runs, the following improvements can be implemented:

### Improvement 1: Automated macOS PF NAT Setup in `Makefile`
Integrate PF rule setup directly into the `sudo-freebsd-net` target. When the user starts the VM with `sudo make sudo-freebsd-net`, the Makefile automatically:
1. Detects the active upstream network interface (e.g. `en0`).
2. Configures `sysctl net.inet.ip.forwarding=1`.
3. Populates the `com.apple/vmm` PF anchor.
4. Cleans up the anchor when the VMM terminates.

```makefile
NET_IF ?= $(shell route get default 2>/dev/null | awk '/interface:/{print $$2}')

sudo-freebsd-net: build
	@echo "[*] Setting up macOS PF NAT forwarding on $(NET_IF)..."
	@sudo sysctl -w net.inet.ip.forwarding=1 >/dev/null
	@printf 'nat on $(NET_IF) from 192.168.18.0/24 to any -> ($(NET_IF))\npass in quick on bridge100 all\npass out quick on $(NET_IF) all\n' | sudo pfctl -a "com.apple/vmm" -f - 2>/dev/null || true
	@sudo ./target/release/vmm \
		--kernel boot/kernel/kernel \
		--initrd boot/kernel/kernel \
		--disk mini-memstick.img \
		--cmdline "boot_single=YES vfs.root.mountfrom=ufs:/dev/vtbd0p2" \
		--cpus 1 --mem 1024 \
		--net vmnet
```

### Improvement 2: Automated In-Guest Network Bootstrap
Add a one-line setup helper alias or bootstrap script in the guest:
```sh
# Inside FreeBSD, create /bin/netup:
echo 'mount -u -w /; mkdir -p /var/db /var/run; dhclient vtnet0; route add default 192.168.18.1; rm -f /etc/resolv.conf; echo "nameserver 1.1.1.1" > /etc/resolv.conf' > /bin/netup
chmod +x /bin/netup
```
Running `netup` configures everything in one second.

### Improvement 3: Built-In User-Mode Networking (SLIRP / Zero-Root)
Implement a user-mode NAT backend (similar to QEMU's `-netdev user` / `slirp`):
* Translates guest TCP/UDP packets into standard unprivileged POSIX socket calls (`connect()`, `send()`, `recv()`) on the macOS host.
* **Benefits**: Does not require `sudo`, `vmnet.framework` entitlements, `bridge100`, or `pfctl` rules. Works out of the box on any standard Mac user account.
