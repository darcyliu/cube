#!/usr/bin/env python3
"""Query or update a running Cube virtio-balloon target."""

from __future__ import annotations

import argparse
import json
import socket
import sys
import time


def request(path: str, payload: dict[str, object]) -> dict[str, object]:
    data = json.dumps(payload, separators=(",", ":")).encode() + b"\n"
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(5)
        client.connect(path)
        client.sendall(data)
        client.shutdown(socket.SHUT_WR)
        response = bytearray()
        while b"\n" not in response:
            chunk = client.recv(4096)
            if not chunk:
                break
            response.extend(chunk)
            if len(response) > 64 * 1024:
                raise RuntimeError("balloon control response is too large")
    if not response:
        raise RuntimeError("balloon controller closed without a response")
    return json.loads(response.split(b"\n", 1)[0])


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Query or update Cube's runtime virtio-balloon target"
    )
    parser.add_argument(
        "--socket",
        default="/tmp/cube-balloon.sock",
        help="balloon control socket (default: %(default)s)",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("status", help="show requested, target, actual, and held pages")
    set_parser = subparsers.add_parser("set", help="set the runtime target in MiB")
    set_parser.add_argument("mib", type=int, help="non-negative target in MiB")
    set_parser.add_argument(
        "--wait",
        action="store_true",
        help="wait for the guest driver, target, actual, and held counts to converge",
    )
    set_parser.add_argument(
        "--timeout",
        type=float,
        default=60.0,
        help="wait timeout in seconds (default: %(default)s)",
    )
    args = parser.parse_args()

    if args.command == "status":
        payload: dict[str, object] = {"status": True}
    else:
        if args.mib < 0:
            parser.error("MiB must be non-negative")
        if args.timeout <= 0:
            parser.error("timeout must be positive")
        payload = {"set_mib": args.mib}

    try:
        response = request(args.socket, payload)
        if args.command == "set" and args.wait:
            deadline = time.monotonic() + args.timeout
            while (
                response.get("ok") is True
                and response.get("converged") is not True
            ):
                if time.monotonic() >= deadline:
                    print(json.dumps(response, indent=2, sort_keys=True))
                    print(
                        "balloonctl: timed out waiting for convergence; "
                        "ensure the guest balloon driver is loaded",
                        file=sys.stderr,
                    )
                    return 1
                time.sleep(0.1)
                response = request(args.socket, {"status": True})
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"balloonctl: {error}", file=sys.stderr)
        return 1
    print(json.dumps(response, indent=2, sort_keys=True))
    return 0 if response.get("ok") is True else 1


if __name__ == "__main__":
    raise SystemExit(main())
