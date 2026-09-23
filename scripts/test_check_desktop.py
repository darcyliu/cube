#!/usr/bin/env python3
"""Protocol accounting tests for the desktop verifier."""
import socket
import struct
import sys
import threading
import unittest

sys.dont_write_bytecode = True
from check_desktop import read, update, validate_events, xi2_events


class DesktopVerifierTests(unittest.TestCase):
    XI2 = """EVENT type 6 (Motion)
    root: 0.00/0.00
EVENT type 6 (Motion)
    root: 1023.00/767.00
EVENT type 15 (RawButtonPress)
    detail: 1
EVENT type 16 (RawButtonRelease)
    detail: 1
EVENT type 15 (RawButtonPress)
    detail: 4
EVENT type 15 (RawButtonPress)
    detail: 5
EVENT type 2 (KeyPress)
    detail: 50
EVENT type 2 (KeyPress)
    detail: 38
EVENT type 3 (KeyRelease)
    detail: 38
EVENT type 3 (KeyRelease)
    detail: 50
EVENT type 2 (KeyPress)
    detail: 37
EVENT type 16 (RawButtonRelease)
    detail: 1
EVENT type 3 (KeyRelease)
    detail: 37
"""

    def test_xi2_event_blocks_and_required_input(self):
        self.assertEqual(xi2_events(self.XI2)[0][0], "Motion")
        validate_events(self.XI2, 1024, 768)

    def test_xi2_requires_disconnect_releases(self):
        text = self.XI2.replace("EVENT type 16 (RawButtonRelease)\n    detail: 1\n", "", 1)
        # Normal master/slave copies must not stand in for a second raw release.
        text += "EVENT type 5 (ButtonRelease)\n    detail: 1\n" * 2
        with self.assertRaisesRegex(AssertionError, "two RawButtonRelease"):
            validate_events(text, 1024, 768)

    def test_xi2_fractional_positions_stay_within_edge_pixels(self):
        text = self.XI2.replace("0.00/0.00", "0.75/0.75")
        text = text.replace("1023.00/767.00", "1023.97/767.97")
        validate_events(text, 1024, 768)

    def test_xi2_requires_both_pointer_edges(self):
        text = self.XI2.replace("    root: 1023.00/767.00", "    root: 800.00/600.00")
        with self.assertRaisesRegex(AssertionError, "Motion root 1023/767"):
            validate_events(text, 1024, 768)

    def test_raw_update_accounts_for_rectangles_and_payload(self):
        client, server = socket.socketpair()
        self.addCleanup(client.close)
        self.addCleanup(server.close)

        def respond():
            request = read(server, 10)
            self.assertEqual(request, struct.pack("!BBHHHH", 3, 1, 0, 0, 8, 6))
            server.sendall(struct.pack("!BBH", 0, 0, 1))
            server.sendall(struct.pack("!HHHHi", 2, 1, 3, 2, 0))
            server.sendall(bytes(3 * 2 * 4))

        worker = threading.Thread(target=respond)
        worker.start()
        result = update(client, 8, 6, True)
        worker.join(timeout=5)
        self.assertFalse(worker.is_alive())
        self.assertEqual(result["rectangles"], 1)
        self.assertEqual(result["payload_bytes"], 24)
        self.assertGreaterEqual(result["seconds"], 0)


if __name__ == "__main__":
    unittest.main()
