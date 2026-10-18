# How QEMU, bhyve, and xhyve Handle Networking, DHCP, and DNS

This note compares the networking backends used by QEMU, FreeBSD bhyve, and xhyve/HyperKit, with emphasis on **who answers DHCP** and **who answers DNS**. It exists to explain why this VMM’s `vmnet` + `bootpd` path needs Option 6 patching and static `ip=`, and why a second DHCP daemon on `bridge100` is the wrong fix.

For this VMM’s own device model, NAT, and troubleshooting, see [networking.md](networking.md).

---

## 1. Two models, not three hypervisors

Every serious hypervisor splits:

1. **The NIC the guest sees** (virtio-net, e1000, …)
2. **The host backend** that actually moves packets

DHCP and DNS almost never live in the hypervisor core. They live in the backend, or they are not implemented at all.

| Model | Packet path | Who answers DHCP? | Who answers DNS? | Root? |
|:------|:------------|:------------------|:-----------------|:------|
| **L2 attach** | Ethernet frames onto tap / bridge / `vmnet` | Host `dhcpd`, LAN router, or Apple `bootpd` | Whatever Option 6 that server sent (often nothing useful) | Usually yes |
| **User-mode stack** | TCP/UDP reconstructed as host `connect()` / `sendto()` | **The backend itself** | **A fake nameserver in the backend** that calls the host resolver | No |

| Hypervisor | Default / common backend | Model |
|:-----------|:-------------------------|:------|
| QEMU `-netdev user` (SLIRP) | in-process NAT | User-mode |
| QEMU `-netdev passt` | passt helper | User-mode |
| QEMU `-netdev tap` / bridge | tap | L2 |
| QEMU `-netdev vmnet-*` (macOS) | `vmnet.framework` | L2 |
| bhyve (classic) | `virtio-net,tapN` + bridge | L2 |
| bhyve `libslirp` | `net/libslirp` | User-mode |
| xhyve `virtio-net` | `vmnet` SHARED_MODE | L2 |
| HyperKit + VPNKit | VPNKit Ethernet socket | User-mode |
| **this VMM** | `vmnet` SHARED_MODE + PF NAT | L2 |

This VMM is the same class as **xhyve `virtio-net`** and **QEMU `vmnet-shared`**. QEMU user-net and VPNKit are a different class.

---

## 2. QEMU

The guest device (`-device virtio-net-pci,netdev=…`) is independent of the backend (`-netdev …`).

### 2.1 User-mode networking (SLIRP) — `-netdev user`

Default if no `-net` is given. No root, no tap, no bridge.

```
guest 10.0.2.15
        │
        ▼  (virtio-net / e1000)
QEMU SLIRP  ─────────────────────────────────────────────┐
  DHCP server   10.0.2.2  (also the NAT gateway)         │
  DNS stub      10.0.2.3  (host getaddrinfo → DNS reply) │
  SMB (optional) 10.0.2.4                                │
        │                                                │
        ▼  host sockets (connect / sendto)               │
     Internet                                            │
```

What the guest DHCP client actually receives:

- Address starting at `10.0.2.15` (`dhcpstart=` can change this)
- Router `10.0.2.2` (the 2nd address in `net=`, default `10.0.2.0/24`)
- DNS `10.0.2.3` (the 3rd address; Option 6 is always present)

**Discover/Request never leave the QEMU process.** SLIRP eats UDP 68→67 and synthesizes Offer/Ack. There is no host `dhcpd` and no Apple `bootpd`.

**DNS queries to `10.0.2.3` also never leave as guest DNS packets onto a bridge.** SLIRP accepts UDP/53, calls the host resolver, and builds a DNS reply. That is why `deb.debian.org` works: the guest is not talking to a gateway that does not speak DNS.

Limits (why this is not “real Ethernet”):

- The guest is behind a firewall; inbound needs `hostfwd=tcp::2222-:22`
- ICMP to the internet often fails; `ping 10.0.2.2` works; use HTTP/SSH to test
- Performance is worse than tap
- Guest-to-guest on the same host is not a shared L2

Useful knobs: `net=192.168.76.0/24`, `dhcpstart=`, `host=`, `dns=`, `hostfwd=`.

This is why a Debian netboot installer “just works” under QEMU user-net: Option 6 is present, and the nameserver actually answers.

### 2.2 passt

`-netdev passt` is a SLIRP replacement (separate helper). Same idea: userspace stack, its own DHCP/DNS, no tap. Not relevant to this VMM except as another data point that “user-mode NAT owns DHCP.”

### 2.3 TAP / bridge

`-netdev tap` puts a real Ethernet tap on the host. QEMU does **not** run DHCP. The guest uses:

- the LAN’s DHCP server if the tap is bridged to `em0`/`en0`, or
- `dnsmasq` / `isc-dhcp-server` on a private bridge, plus host NAT

This is the same operational model as classic bhyve.

### 2.4 macOS vmnet — `-netdev vmnet-shared` / `vmnet-host` / `vmnet-bridged`

Same Apple API this VMM uses. See [§5 Apple vmnet modes](#5-apple-vmnet-modes-shared--host--bridged). `vmnet-shared` creates a NAT subnet (historically `192.168.64.0/24`) and macOS **`bootpd`** hands out leases. `bootpd` still omits DHCP Option 6. QEMU does not patch those packets; guests that DHCP here have the same DNS hole unless the user sets resolvers by hand or uses `-netdev user` instead.

---

## 3. bhyve (FreeBSD)

`bhyve` attaches `virtio-net` (or e1000) to a **tap** or **netgraph** hook. The hypervisor does not include a DHCP server in the default path.

```
bhyve -s 4,virtio-net,tap0 …
              │
              ▼
           tap0  ──►  bridge0  ──►  em0  (LAN)
                 or   bridge0  ──►  pf NAT  (private subnet)
```

### 3.1 Bridged to the LAN

Guest DHCP is the house router / pfSense. DNS is whatever that server put in Option 6. bhyve is not involved. This is the common `vm-bhyve` “public switch” setup (`ifconfig bridge0 addm em0 addm tap0`).

Wi-Fi cannot be a bridge member on most stacks (MAC spoofing). Then you must NAT, not bridge.

### 3.2 Private bridge + NAT

Host:

- `bridge0` address `10.0.0.1/24`
- `pf` `nat on em0 from 10.0.0.0/24 to any -> (em0)`
- **`dhcpd` or `dnsmasq` on the host**, with `option domain-name-servers …`

The DHCP/DNS servers are ordinary host daemons. If you forget `option domain-name-servers`, Linux guests fail the same way Alpine failed here before `ip=`.

### 3.3 libslirp (optional)

Newer `bhyve(8)` can attach `virtio-net` to `net/libslirp`:

- `open` — outbound NAT via host sockets
- `hostfwd=` — inbound port forwards
- **the backend provides DHCP and DNS to the guest**

That is QEMU user-net, ported. It is not the handbook default; the handbook is still tap+bridge.

---

## 4. xhyve / HyperKit

xhyve is bhyve on macOS `Hypervisor.framework`. Two generations of networking.

### 4.1 Original xhyve `virtio-net` — vmnet (this VMM’s cousin)

```
guest virtio-net
        │
        ▼
xhyve vmnet SHARED_MODE
        │
        ▼
bridge100  +  bootpd   (192.168.64.0/24 by default)
        │
        ▼
host NAT (variable quality across macOS versions)
```

- Needs root (or the vmnet entitlement)
- IP is stable if you pass `-U uuid` (lease keyed by UUID / MAC in `/var/db/dhcpd_leases`)
- Optional `virtio-tap,tapX` if you already have a tap
- **No in-process DHCP, no DNS stub.** Option 6 is whatever `bootpd` sent — which is nothing

This is why xhyve/docker-machine-driver-xhyve users hit empty `resolv.conf`. It is the same bug as this VMM.

### 4.2 HyperKit + VPNKit (Docker Desktop, LinuxKit)

Docker gave up on vmnet for the default path. VPNKit is a userspace Ethernet translator sitting on a Unix socket (`virtio-vpnkit,path=/tmp/ethernet`):

```
guest DHCP Discover
        │
        ▼
VPNKit  — answers DHCP itself (e.g. 192.168.65.0/24, gw .1)
        — UDP/53 intercepted, forwarded via host stub resolver
        — TCP reconstructed as host connect()
        │
        ▼
Looks like the Docker / HyperKit process to VPNs and the firewall
```

LinuxKit documents:

- `192.168.65.1` — VPNKit gateway
- `192.168.65.2` — Docker-for-Mac VM
- further addresses from VPNKit DHCP
- VMs are **not** reachable by IP from the Mac without vmnet (which needs root); access is port-forward / socket

VPNKit exists because bridging Ethernet on macOS fights VPN software (same routing/DNS/firewall tables). Reconstructing TCP as host sockets makes guest traffic inherit the host VPN.

---

## 5. Apple vmnet modes (shared / host / bridged)

`vmnet.framework` has three operating modes. QEMU exposes them as `-netdev vmnet-shared`, `vmnet-host`, and `vmnet-bridged`. This VMM hard-codes **shared**.

`--net vmnet` in [`src/devices/virtio/net.rs`](../src/devices/virtio/net.rs) sets:

```c
vmnet_operation_mode = VMNET_SHARED_MODE  /* 1000 */
```

There is no host-only or bridged backend yet.

| Mode | Guest IP | Who answers DHCP? | Internet | Isolation |
|:-----|:---------|:------------------|:---------|:----------|
| **Shared** (`VMNET_SHARED_MODE`, this VMM) | Private subnet. Apple’s default is often `192.168.64.0/24`; this VMM’s NAT/scripts assume **`192.168.18.0/24`**, gateway `192.168.18.1` | macOS `bootpd` on `bridge100` | NAT through the Mac (plus PF `com.apple/cube` on current macOS) | Guest is **not** on the LAN |
| **Host** (`VMNET_HOST_MODE`) | Same style of private subnet | `bootpd` again, or none if a net UUID isolates the network | **No** — host and other host-mode VMs only | Stronger isolation |
| **Bridged** (`VMNET_BRIDGED_MODE`) | Same subnet as the Mac (`en0`) | **The home/office router**, not `bootpd` | Direct on the LAN | Guest looks like another machine on Wi-Fi/Ethernet |

Shared still needs extra PF NAT (`nat on en0 from 192.168.18.0/24 …`) on modern macOS; that is why `Makefile` / `debian.sh` / `alpine.sh` run `pfctl`. Bridged does not use that NAT, because the guest already has a LAN address.

### Should this VMM switch to bridged?

**No, not as the default.** Shared is the right default (same as QEMU `vmnet-shared` and Parallels “Shared”).

Bridged is **not** a general fix for Debian “Bad archive mirror”:

- It can help DNS **if** the LAN DHCP server sends Option 6 (most home routers do). That is the one real advantage versus Apple `bootpd`.
- Wi-Fi often **cannot** be a proper L2 bridge (same limitation as bhyve + `wlan0`). Bridged on a Mac is reliable mainly on Ethernet `en0`.
- You must pass the physical ifname (`vmnet_shared_interface_name_key`).
- The guest gets a LAN IP, not `192.168.18.2`. Alpine/Debian `ip=`, PF rules, and SSH forward (`192.168.18.2:22`) would all break until rewritten.
- The VM is visible on the home network (less isolation).
- Still needs `sudo` / `com.apple.vm.networking`.

Use bridged only as an **optional** backend later (`--net bridged:en0`) for “this VM should be a peer on my LAN.” Keep shared for `make alpine` / `make debian` / `make freebsd-net`.

Host mode is only useful for VM-to-VM with no internet.

The Debian installer issue is still: shared-mode `bootpd` has no DNS, and wget prefers IPv6. Bridged might dodge DNS on Ethernet; it does not replace static `ip=` + `ipv6.disable=1` for the default path.

---

## 6. DHCP Option 6 and DNS, side by side

| Stack | DHCP Offer includes DNS? | Where do queries go? | Guest `resolv.conf` if DHCP-only |
|:------|:-------------------------|:---------------------|:---------------------------------|
| QEMU SLIRP | Yes (`10.0.2.3`) | SLIRP stub → host resolver | nameserver 10.0.2.3 |
| bhyve libslirp | Yes | libslirp stub | nameserver in the slirp net |
| VPNKit | Yes | VPNKit stub → host resolver (VPN-aware) | nameserver on 192.168.65.0/24 |
| bhyve tap + dnsmasq/dhcpd | Only if you configured it | The server you named | whatever you put in dhcpd.conf |
| bhyve tap + LAN DHCP | Yes, from the router | Router or ISP DNS | usually fine |
| xhyve / QEMU vmnet / **this VMM** | **No** (`bootpd` omits Option 6) | Guest may try the gateway `:53`, which does not speak DNS | empty / missing → musl `getaddrinfo` fails |

Apple `bootpd` on `vmnet` SHARED_MODE:

- Sends Option 1 (mask), 3 (router = bridge gateway), 51 (lease)
- Does **not** send Option 6
- The gateway (`192.168.64.1` or `192.168.18.1`) does **not** proxy UDP/53

That is a host-stack limitation, not a virtio-net bug.

---

## 7. What this VMM does today

Always **`VMNET_SHARED_MODE`**. See [§5](#5-apple-vmnet-modes-shared--host--bridged) for why not bridged by default.

```
Guest dhclient / udhcpc / systemd-networkd
        │
        ▼
VirtIO-MMIO net  TX ──► vmnet_write() ──► bootpd (IP + router, no Option 6)
                 RX ◄── vmnet_read()  ◄── bootpd Offer/Ack
                           │
                           └── patch_dhcp_response() injects 1.1.1.1 / 8.8.8.8
        │
        ▼
bridge100 + pf NAT (com.apple/cube) ──► en0 ──► Internet
```

Plus Linux bootargs (Alpine / Debian), which **bypass DHCP entirely**:

```
ip=192.168.18.2::192.168.18.1:255.255.255.0::eth0:none:1.1.1.1:8.8.8.8
ipv6.disable=1
```

`ipv6.disable=1` is unrelated to DHCP: Debian-installer wget prefers AAAA for `deb.debian.org`, and vmnet NAT is IPv4-only.

A **second DHCP server on `bridge100`** would race `bootpd` (two Offers, two server-ids). QEMU/VPNKit do not do that; they never send Discover to `bootpd`.

---

## 8. If this VMM grows a user-mode backend

That work is QEMU SLIRP / VPNKit / bhyve libslirp, not “a lightweight DHCP daemon.”

A user-mode backend would:

1. Intercept DHCP Discover/Request on the virtio TX path and never `vmnet_write` them
2. Synthesize Offer/Ack with Option 6 pointing at a **stub DNS IP it owns** (or at `1.1.1.1` if it does not proxy)
3. Optionally intercept UDP/53 to that stub and call host `getaddrinfo` (QEMU `10.0.2.3`, VPNKit)
4. Translate guest TCP/UDP to host sockets (true slirp) **or** keep vmnet for data plane and only steal DHCP/DNS (a smaller intercept)

Until then, the vmnet recipe is: static `ip=` + public DNS for Linux, Option 6 patch for guests that DHCP, PF NAT for forwarding. See [networking.md](networking.md) Improvement 3.

---

## 9. References

- [QEMU Network emulation](https://www.qemu.org/docs/master/system/devices/net.html) — user-mode diagram (`10.0.2.15` / `.2` / `.3`)
- [QEMU wiki: User Networking (SLIRP)](https://wiki.qemu.org/Documentation/Networking)
- [bhyve(8)](https://www.freebsd.org/cgi/man.cgi?query=bhyve) — `virtio-net` tap, netgraph, and `libslirp` (`open`, `hostfwd`, DHCP/DNS)
- [vm-bhyve Virtual Network Interfaces](https://github.com/churchers/vm-bhyve/wiki/Virtual-Network-Interfaces)
- [xhyve README (HyperKit tree)](https://github.com/moby/hyperkit/blob/master/README.xhyve.md) — `virtio-net` vmnet vs `virtio-tap`
- [VPNKit README](https://github.com/moby/vpnkit) — Ethernet → host sockets; DNS/NTP/TCP interception
- [LinuxKit HyperKit platform](https://github.com/linuxkit/linuxkit/blob/master/docs/platform-hyperkit.md) — VPNKit DHCP on `192.168.65.0/24`
- [Apple vmnet](https://developer.apple.com/documentation/vmnet) — shared / host / bridged operating modes
- [QEMU `-netdev vmnet-*`](https://www.qemu.org/docs/master/system/invocation.html) — `vmnet-shared`, `vmnet-host`, `vmnet-bridged`
