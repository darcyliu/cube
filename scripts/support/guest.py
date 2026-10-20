"""Guest network selection and atomic UART mailbox publishing."""
import re
import time


def publish(path, command):
    deadline = time.monotonic() + 10
    while path.exists():
        if time.monotonic() > deadline:
            raise AssertionError("UART command was not consumed")
        time.sleep(0.05)
    pending = path.with_suffix(".next")
    pending.write_text(command + "\n")
    pending.replace(path)


def guest_net(sudo, requested):
    mode = "vmnet" if sudo and requested is None else (requested or "vmnet-proxy")
    iface = mode.removeprefix("bridge:")
    valid = mode in ("vmnet-proxy", "vmnet", "bridge") or (
        mode.startswith("bridge:") and iface != mode
        and re.fullmatch(r"[A-Za-z][A-Za-z0-9]{0,15}", iface) is not None)
    return mode, valid


def in_process_net(mode):
    return mode == "vmnet" or mode == "bridge" or mode.startswith("bridge:")


