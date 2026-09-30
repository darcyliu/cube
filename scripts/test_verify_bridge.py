#!/usr/bin/env python3
"""Interface selection and command text. No ping, sudo, or hypervisor."""
import importlib.util
import ipaddress
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    "verify_bridge", Path(__file__).with_name("verify-bridge.py"))
bridge = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bridge)

PORTS = """
Hardware Port: Ethernet Adapter (en4)
Device: en4

Hardware Port: USB 10/100/1000 LAN
Device: en9

Hardware Port: Wi-Fi
Device: en0

Hardware Port: Thunderbolt Bridge
Device: bridge0
"""

IFCONFIG = """
en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
	inet 192.168.1.97 netmask 0xffffff00 broadcast 192.168.1.255
	status: active
en4: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
	status: inactive
en9: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
	inet 10.1.2.15 netmask 0xffffff00 broadcast 10.1.2.255
	inet 169.254.1.1 netmask 0xffff0000 broadcast 169.254.255.255
	status: active
bridge0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
	inet 192.168.8.1 netmask 0xffffff00 broadcast 192.168.8.255
	status: active
"""


class SelectionTests(unittest.TestCase):
    def setUp(self):
        self.ports = bridge.parse_hardware_ports(PORTS)
        self.ifaces = bridge.parse_ifconfig(IFCONFIG)

    def test_ethernet_wins_over_the_wifi_default_route(self):
        name, label, host, warning = bridge.select_interface(self.ports, self.ifaces, "en0")
        self.assertEqual((name, label, str(host)), ("en9", "USB 10/100/1000 LAN", "10.1.2.15/24"))
        self.assertIsNone(warning)

    def test_wifi_only_still_selects_the_default_route(self):
        wifi_only = {"en0": self.ifaces["en0"], "en4": self.ifaces["en4"]}
        name, label, host, warning = bridge.select_interface(self.ports, wifi_only, "en0")
        self.assertEqual((name, label, str(host)), ("en0", "Wi-Fi", "192.168.1.97/24"))
        self.assertIn("DHCP server on en0", warning)
        self.assertNotIn("cannot bridge", warning)

    def test_requested_wifi_is_selected_with_a_warning(self):
        name, label, host, warning = bridge.select_interface(self.ports, self.ifaces, "en0", "en0")
        self.assertEqual((name, label, str(host)), ("en0", "Wi-Fi", "192.168.1.97/24"))
        self.assertIn("DHCP server on en0", warning)
        self.assertNotIn("cannot bridge", warning)

    def test_requested_ethernet_without_a_carrier_is_rejected(self):
        with self.assertRaises(bridge.BridgeError) as caught:
            bridge.select_interface(self.ports, self.ifaces, "en0", "en4")
        self.assertIn("inactive", str(caught.exception))

    def test_guest_prefers_77_then_skips_answers(self):
        host = ipaddress.IPv4Interface("10.1.2.15/24")
        gateway = ipaddress.IPv4Address("10.1.2.1")
        candidates = bridge.guest_candidates(host, gateway)
        self.assertEqual(candidates[0], ipaddress.IPv4Address("10.1.2.77"))
        self.assertNotIn(host.ip, candidates)
        self.assertNotIn(gateway, candidates)
        used = {ipaddress.IPv4Address("10.1.2.77")}
        guest = bridge.find_guest(candidates, lambda address: address in used)
        self.assertEqual(guest, ipaddress.IPv4Address("10.1.2.254"))

    def test_arp_mac_means_the_address_is_in_use(self):
        self.assertIsNotNone(bridge.MAC_ADDRESS.search(
            "? (192.168.1.254) at 60:d8:a4:3e:fc:18 on en0 ifscope [ethernet]"))
        self.assertIsNone(bridge.MAC_ADDRESS.search(
            "? (192.168.1.77) at (incomplete) on en0 ifscope [ethernet]"))
        self.assertIsNone(bridge.MAC_ADDRESS.search(
            "192.168.1.250 (192.168.1.250) -- no entry"))

    def test_command_uses_the_bridged_interface_and_its_subnet(self):
        host = ipaddress.IPv4Interface("10.1.2.15/24")
        text = bridge.format_command(
            "en9", ipaddress.IPv4Address("10.1.2.77"), host, "disc.iso", "firmware/QEMU_EFI.fd")
        self.assertEqual(text, "\n".join([
            "sudo python3 -B scripts/verify.py --suite full --bridge en9 --keep-root \\",
            "  --iso disc.iso \\",
            "  --firmware firmware/QEMU_EFI.fd \\",
            "  --guest-cidr 10.1.2.77/24 --host-ip 10.1.2.15",
        ]))

    def test_default_route_parser_ignores_a_non_ipv4_gateway(self):
        text = "gateway: fe80::1%en0\ninterface: en0\n"
        self.assertEqual(bridge.parse_default_route(text), ("en0", None))


if __name__ == "__main__":
    unittest.main()
