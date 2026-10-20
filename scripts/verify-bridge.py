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
sys.path.insert(0, str(REPO / "scripts"))

from support.network import (
    MAX_PROBES, MAC_ADDRESS, SKIP_PREFIXES, BridgeError,
    parse_hardware_ports, parse_ifconfig, parse_default_route, wifi_port,
    usable_interface, select_interface, wifi_warning, guest_candidates,
    find_guest, address_in_use, detect_lan_network, detect_vmnet_subnet
)



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
