#!/usr/bin/env python3
"""Print a verify.py full-suite bridge command for this Mac's current LAN.

An active Ethernet port is used when one has an address. Otherwise the
default route is used, including Wi-Fi. vmnet accepts a bridged start on
Wi-Fi; guests take addresses from the DHCP server on that interface. The
guest address is an unused host on that subnet.

  python3 -B scripts/verify-bridge.py
  python3 -B scripts/verify-bridge.py --iface en9 --guest-ip 192.168.1.80

The command is printed on stdout. Diagnostics go to stderr.
"""
import argparse
import ipaddress
from pathlib import Path
import re
import shlex
import subprocess
import sys

REPO = Path(__file__).resolve().parent.parent
MAX_PROBES = 32
MAC_ADDRESS = re.compile(r"\bat\s+[0-9a-f]{1,2}(?::[0-9a-f]{1,2}){5}\b", re.IGNORECASE)
SKIP_PREFIXES = ("lo", "gif", "stf", "awdl", "llw", "utun", "bridge", "ap", "anpi", "nan")


class BridgeError(Exception):
    pass


def parse_hardware_ports(text):
    """Map a device name to its Hardware Port label."""
    ports = {}
    label = None
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("Hardware Port:"):
            label = line.split(":", 1)[1].strip()
        elif line.startswith("Device:") and label:
            device = line.split(":", 1)[1].strip()
            if device:
                ports[device] = label
            label = None
    return ports


def parse_ifconfig(text):
    """IPv4 addresses and link status for each interface."""
    ifaces = {}
    current = None
    for line in text.splitlines():
        if line and not line[0].isspace():
            current = line.split(":", 1)[0]
            ifaces[current] = {"status": None, "addresses": []}
            continue
        if current is None:
            continue
        stripped = line.strip()
        if stripped.startswith("inet "):
            parts = stripped.split()
            address = parts[1]
            if "netmask" not in parts:
                continue
            mask = parts[parts.index("netmask") + 1]
            prefix = int(mask, 16).bit_count() if mask.startswith("0x") else int(mask)
            interface = ipaddress.IPv4Interface(f"{address}/{prefix}")
            if not interface.ip.is_link_local:
                ifaces[current]["addresses"].append(interface)
        elif stripped.startswith("status:"):
            ifaces[current]["status"] = stripped.split(":", 1)[1].strip()
    return ifaces


def parse_default_route(text):
    interface = gateway = None
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("interface:"):
            interface = line.split(":", 1)[1].strip()
        elif line.startswith("gateway:"):
            raw = line.split(":", 1)[1].strip()
            try:
                gateway = ipaddress.IPv4Address(raw)
            except ipaddress.AddressValueError:
                gateway = None
    return interface, gateway


def wifi_port(label):
    return label.strip().casefold() == "wi-fi"


def usable_interface(name, info, ports):
    if name.startswith(SKIP_PREFIXES) or not info["addresses"]:
        return False
    if wifi_port(ports.get(name, "")):
        return False
    return info["status"] in (None, "active")


def select_interface(ports, ifaces, default_iface, requested=None):
    """Pick an interface that has an IPv4 address.

    An active Ethernet port wins. Otherwise the default route is used, including
    Wi-Fi. The warning only names that the port is Wi-Fi.
    """
    if requested:
        info = ifaces.get(requested)
        if info is None:
            raise BridgeError(f"interface {requested} was not found")
        label = ports.get(requested, requested)
        if info["status"] not in (None, "active"):
            raise BridgeError(f"{requested} ({label}) is {info['status']}")
        if not info["addresses"]:
            raise BridgeError(f"{requested} ({label}) has no IPv4 address")
        return requested, label, info["addresses"][0], wifi_warning(ports, requested)

    def rank(name):
        label = ports.get(name, "")
        ethernet = "ethernet" in label.casefold() or "lan" in label.casefold()
        return (name != default_iface, not ethernet, name)

    chosen = sorted(
        (name for name, info in ifaces.items() if usable_interface(name, info, ports)),
        key=rank,
    )
    if chosen:
        name = chosen[0]
        return name, ports.get(name, name), ifaces[name]["addresses"][0], None
    if default_iface and ifaces.get(default_iface, {}).get("addresses"):
        info = ifaces[default_iface]
        return (
            default_iface,
            ports.get(default_iface, default_iface),
            info["addresses"][0],
            wifi_warning(ports, default_iface),
        )
    raise BridgeError("no interface with an IPv4 address is up")


def wifi_warning(ports, name):
    if not wifi_port(ports.get(name, "")):
        return None
    return f"{name} is Wi-Fi. Guests take IPv4 addresses from the DHCP server on {name}."


def guest_candidates(host, gateway):
    """Host addresses in the subnet, trying .77 before the top of the range."""
    network = host.network
    reserved = {host.ip}
    if gateway is not None and gateway in network and gateway != network.network_address:
        reserved.add(gateway)
    hosts = [addr for addr in network.hosts() if addr not in reserved]
    preferred = ipaddress.IPv4Address(int(network.network_address) + 77)
    ordered = []
    if preferred in hosts:
        ordered.append(preferred)
    ordered.extend(addr for addr in reversed(hosts) if addr not in ordered)
    return ordered


def find_guest(candidates, probe, requested=None, limit=MAX_PROBES):
    if requested is not None:
        if probe(requested):
            raise BridgeError(f"{requested} answered on the LAN; choose another --guest-ip")
        return requested
    checked = 0
    for address in candidates:
        if checked >= limit:
            break
        checked += 1
        if not probe(address):
            return address
    raise BridgeError(
        f"no unused address in the first {checked} candidates. Pass --guest-ip"
    )


def address_in_use(address):
    """True when the address answers ICMP or has a completed ARP entry."""
    ip = str(address)
    ping = subprocess.run(
        ["ping", "-c", "1", "-W", "300", ip],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=2,
        check=False,
    )
    if ping.returncode == 0:
        return True
    arp = subprocess.run(
        ["arp", "-n", ip],
        capture_output=True,
        text=True,
        timeout=2,
        check=False,
    )
    return MAC_ADDRESS.search(f"{arp.stdout}\n{arp.stderr}") is not None


def format_command(iface, guest, host, iso, firmware):
    lines = [
        "sudo python3 -B scripts/verify.py --suite full "
        f"--bridge {shlex.quote(iface)} --keep-root",
        f"--iso {shlex.quote(iso)}",
        f"--firmware {shlex.quote(firmware)}",
        f"--guest-cidr {guest}/{host.network.prefixlen} --host-ip {host.ip}",
    ]
    return " \\\n  ".join(lines)


def display_path(path):
    resolved = path.expanduser().resolve()
    try:
        return str(resolved.relative_to(REPO))
    except ValueError:
        return str(resolved)


def default_iso():
    preferred = REPO / "FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso"
    if preferred.is_file():
        return preferred
    found = sorted(REPO.glob("FreeBSD-*-disc1.iso"))
    if len(found) == 1:
        return found[0]
    if not found:
        raise BridgeError("pass --iso; no FreeBSD disc1 image is in the repo root")
    names = ", ".join(path.name for path in found)
    raise BridgeError(f"pass --iso; several disc1 images are present: {names}")


def capture(command):
    result = subprocess.run(command, capture_output=True, text=True, timeout=5, check=False)
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise BridgeError(f"{command[0]} failed: {detail}")
    return result.stdout


def build(requested_iface=None, requested_guest=None, iso=None, firmware=None, probe=None):
    ports = parse_hardware_ports(capture(["networksetup", "-listallhardwareports"]))
    ifaces = parse_ifconfig(capture(["ifconfig", "-a"]))
    default_iface, gateway = parse_default_route(capture(["route", "-n", "get", "default"]))
    name, label, host, warning = select_interface(ports, ifaces, default_iface, requested_iface)
    if warning:
        print(f"verify-bridge: {warning}", file=sys.stderr)
    if requested_guest is not None and requested_guest not in host.network:
        raise BridgeError(f"{requested_guest} is not on {host.network}, the subnet of {name}")
    if requested_guest == host.ip:
        raise BridgeError(f"{requested_guest} is this Mac's address on {name}")
    guest = find_guest(
        guest_candidates(host, gateway),
        probe or address_in_use,
        requested_guest,
    )
    iso_path = Path(iso) if iso else default_iso()
    if not iso_path.expanduser().is_file():
        raise BridgeError(f"ISO was not found: {iso_path}")
    firmware_path = Path(firmware) if firmware else REPO / "firmware" / "QEMU_EFI.fd"
    if not firmware_path.expanduser().is_file():
        raise BridgeError(f"firmware was not found: {firmware_path}")
    print(
        f"verify-bridge: {name} ({label}) {host}, guest {guest}/{host.network.prefixlen}",
        file=sys.stderr,
    )
    return format_command(name, guest, host, display_path(iso_path), display_path(firmware_path))


def parse_guest(value):
    if "/" in value:
        interface = ipaddress.IPv4Interface(value)
        return interface.ip
    return ipaddress.IPv4Address(value)


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--iface", help="Ethernet device to bridge, such as en9")
    parser.add_argument("--guest-ip", type=parse_guest,
                        help="unused guest address on that subnet; probed when omitted")
    parser.add_argument("--iso", type=Path, help="ARM64 disc1 ISO (default: FreeBSD 14.4 in the repo)")
    parser.add_argument("--firmware", type=Path, help="EDK2 firmware (default: firmware/QEMU_EFI.fd)")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    try:
        print(build(args.iface, args.guest_ip, args.iso, args.firmware))
    except BridgeError as error:
        print(f"verify-bridge: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
