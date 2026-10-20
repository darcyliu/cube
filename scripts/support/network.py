"""Network inspection, LAN/bridge selection, and vmnet subnet detection."""
import ipaddress
from pathlib import Path
import re
import subprocess

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


def detect_lan_network(requested_iface=None, requested_guest=None, probe=None):
    """Auto-detect the host's LAN interface, IP, and an unused guest IP."""
    ports_proc = subprocess.run(["networksetup", "-listallhardwareports"], capture_output=True, text=True, check=False)
    ports = parse_hardware_ports(ports_proc.stdout) if ports_proc.returncode == 0 else {}
    ifaces_proc = subprocess.run(["ifconfig", "-a"], capture_output=True, text=True, check=False)
    ifaces = parse_ifconfig(ifaces_proc.stdout) if ifaces_proc.returncode == 0 else {}

    default_iface = None
    gateway = None
    route_proc = subprocess.run(["route", "-n", "get", "default"], capture_output=True, text=True, check=False)
    if route_proc.returncode == 0:
        default_iface, gateway = parse_default_route(route_proc.stdout)
    else:
        netstat_proc = subprocess.run(["netstat", "-rn", "-f", "inet"], capture_output=True, text=True, check=False)
        if netstat_proc.returncode == 0:
            for line in netstat_proc.stdout.splitlines():
                if line.startswith("default"):
                    parts = line.split()
                    if len(parts) >= 4:
                        try:
                            gateway = ipaddress.IPv4Address(parts[1])
                        except ipaddress.AddressValueError:
                            pass
                        default_iface = parts[-1]
                        break

    name, label, host, warning = select_interface(ports, ifaces, default_iface, requested=requested_iface)
    candidates = guest_candidates(host, gateway)
    guest = find_guest(candidates, probe or address_in_use, requested_guest)
    guest_cidr = ipaddress.IPv4Interface(f"{guest}/{host.network.prefixlen}")
    return name, label, guest_cidr, host.ip, warning


def detect_vmnet_subnet():
    """Detect the IPv4 subnet of bridge100 if present, or fallback to default 192.168.18.0/24."""
    proc = subprocess.run(["ifconfig", "bridge100"], capture_output=True, text=True, check=False)
    if proc.returncode == 0:
        ifaces = parse_ifconfig(proc.stdout)
        if "bridge100" in ifaces and ifaces["bridge100"]["addresses"]:
            host_interface = ifaces["bridge100"]["addresses"][0]
            host_ip = host_interface.ip
            net = host_interface.network
            guest_ip = ipaddress.IPv4Address(int(net.network_address) + 77)
            if guest_ip in net.hosts() and guest_ip != host_ip:
                return ipaddress.IPv4Interface(f"{guest_ip}/{net.prefixlen}"), host_ip
    return ipaddress.IPv4Interface("192.168.18.77/24"), ipaddress.IPv4Address("192.168.18.1")
