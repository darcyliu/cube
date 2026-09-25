#!/usr/bin/env python3
"""Launch and manage named VMM instances from JSON configuration files."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import time
import uuid

REPO = Path(__file__).resolve().parent.parent
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]{0,47}\Z")


def require(condition, message):
    if not condition:
        raise ValueError(message)


def path_from(value, base):
    require(isinstance(value, str) and value, "expected a nonempty path")
    return str((base / value).expanduser().resolve())


def address(value):
    require(isinstance(value, str) and value, "expected a listener address")
    host, sep, port = value.rpartition(":")
    require(sep and host and port.isdecimal() and 0 < int(port) < 65536,
            f"invalid listener address: {value}")
    return value


def listener_resource(value):
    return "listen-port:" + address(value).rpartition(":")[2]


def read_config(path):
    config = json.loads(path.read_text())
    require(isinstance(config, dict), "configuration must be a JSON object")
    name = config.get("name")
    require(isinstance(name, str) and NAME.fullmatch(name),
            "name must contain 1–48 letters, digits, underscores, or hyphens")
    return config


def state_paths(config_path, state_root):
    name = read_config(config_path)["name"]
    root = state_root.expanduser().resolve()
    return root, root / name


def control_socket_path(state_dir):
    # macOS limits AF_UNIX pathnames to 103 bytes. Artifact/state roots may be
    # much longer, so the pathname must not include the state directory.
    digest = hashlib.sha256(str(state_dir.resolve()).encode()).hexdigest()[:24]
    return Path("/tmp") / f"vmmctl-{os.getuid()}" / (digest + ".sock")


def ensure_control_root(control):
    root = control.parent
    root.mkdir(mode=0o700, exist_ok=True)
    stat = root.lstat()
    require(root.is_dir() and not root.is_symlink() and stat.st_uid == os.getuid(),
            f"control socket directory is not owned by this user: {root}")
    root.chmod(0o700)


def launch_spec(config_path, state_dir, args):
    config = read_config(config_path)
    base = config_path.parent
    binary = path_from(args.binary or config.get("binary", str(REPO / "target/debug/vmm")),
                       Path.cwd() if args.binary else base)
    require(Path(binary).is_file(), f"VMM binary does not exist: {binary}")
    boot = config.get("boot", {})
    require(isinstance(boot, dict), "boot must be an object")
    mode = boot.get("mode")
    require(mode in ("uefi", "freebsd"), "boot.mode must be uefi or freebsd")
    command = [binary]
    if mode == "uefi":
        command += ["--uefi", path_from(boot.get("firmware"), base)]
    else:
        command += ["--freebsd", path_from(boot.get("kernel"), base)]
        if boot.get("multiuser", True):
            command += ["--multiuser"]
    if "root" in boot:
        require(isinstance(boot["root"], str) and boot["root"], "boot.root must be nonempty")
        command += ["--root", boot["root"]]
    if "bootargs" in boot:
        require(isinstance(boot["bootargs"], str), "boot.bootargs must be a string")
        command += ["--bootargs", boot["bootargs"]]
    modules = boot.get("modules", [])
    require(isinstance(modules, list), "boot.modules must be a list")
    for module in modules:
        command += ["--module", path_from(module, base)]
    ram = args.ram if args.ram is not None else config.get("ram_mb", 1024)
    cpus = args.cpus if args.cpus is not None else config.get("cpus", 1)
    require(type(ram) is int and 128 <= ram <= 65536, "ram_mb must be 128–65536")
    require(type(cpus) is int and 1 <= cpus <= 16, "cpus must be 1–16")
    command += ["--ram", str(ram), "--cpus", str(cpus)]

    disks = args.disk if args.disk is not None else config.get("disks", [])
    require(isinstance(disks, list) and disks, "disks must be a nonempty list")
    resources = []
    for item in disks:
        if isinstance(item, str):
            item = {"path": item}
        require(isinstance(item, dict), "each disk must be a path or object")
        disk_base = Path.cwd() if args.disk is not None else base
        disk = path_from(item.get("path"), disk_base)
        require(Path(disk).is_file(), f"disk does not exist: {disk}")
        cow, read_only = item.get("cow", False), item.get("read_only", False)
        require(type(cow) is bool and type(read_only) is bool,
                "disk cow/read_only must be booleans")
        require(not (cow and read_only), "a disk cannot be cow and read_only")
        suffix = (",cow" if cow else ",ro" if read_only else "")
        command += ["--disk", disk + suffix]
        if not read_only:
            resources.append("disk:" + disk)
            if cow:
                resources.append("disk:" + disk + ".cow")

    cli_shares = getattr(args, "share", None)
    cli_rw = getattr(args, "share_rw", None)
    share_items = ([{"tag": tag, "path": path, "read_only": ro}
                    for items, ro in ((cli_shares or [], True), (cli_rw or [], False))
                    for tag, path in items] if cli_shares is not None or cli_rw is not None
                   else config.get("shares", []))
    require(isinstance(share_items, list), "shares must be a list")
    shares, tags = [], set()
    for item in share_items:
        require(isinstance(item, dict), "each share must be an object")
        tag, ro = item.get("tag"), item.get("read_only", True)
        require(isinstance(tag, str) and re.fullmatch(r"[A-Za-z0-9_.-]{1,120}", tag),
                "share tag must be 1–120 ASCII letters, digits, '.', '_' or '-'")
        require(tag not in tags, "duplicate share tag: " + tag)
        require(type(ro) is bool, "share read_only must be a boolean")
        path = path_from(item.get("path"), Path.cwd() if cli_shares is not None or cli_rw is not None else base)
        require(Path(path).is_dir(), "share directory does not exist: " + path)
        tags.add(tag)
        shares.append({"tag": tag, "path": path, "read_only": ro})
        command += ["--share" if ro else "--share-rw", tag, path]
    require(max(0, len(disks) - 1) + len(shares) <= 64, "too many disks/shared folders")

    network = config.get("network", {})
    require(isinstance(network, dict), "network must be an object")
    net_mode = args.net or network.get("mode", "none")
    require(isinstance(net_mode, str) and (net_mode in ("none", "mock", "vmnet", "bridge")
            or net_mode.startswith(("bridge:", "tap:"))), "invalid network mode")
    command += ["--net", net_mode]
    mac = args.mac or network.get("mac")
    if mac is None:
        digest = hashlib.sha256(config["name"].encode()).digest()
        mac = "52:54:%02x:%02x:%02x:%02x" % tuple(digest[:4])
    require(isinstance(mac, str) and re.fullmatch(r"[0-9a-fA-F]{2}(:[0-9a-fA-F]{2}){5}", mac),
            "network.mac must be six hexadecimal bytes")
    if net_mode != "none":
        command += ["--mac", mac]
        resources.append("mac:" + mac.lower())
    ssh_port = args.ssh_port if args.ssh_port is not None else network.get("ssh_port")
    if ssh_port is not None:
        require(type(ssh_port) is int and 0 < ssh_port < 65536, "invalid ssh_port")
        require(net_mode not in ("none", "mock") and not net_mode.startswith("tap:"),
                "ssh_port needs vmnet or bridge")
        command += ["--ssh-port", str(ssh_port)]
        resources.append("listen-port:" + str(ssh_port))
    forwards = args.forward if args.forward is not None else network.get("forwards", [])
    require(isinstance(forwards, list), "network.forwards must be a list")
    for rule in forwards:
        require(isinstance(rule, str) and "=" in rule, "invalid forward rule")
        command += ["--forward", rule]
        resources.append(listener_resource(rule.split("=", 1)[0]))

    vnc = config.get("vnc")
    if args.vnc is not None:
        vnc = {**(vnc or {}), "bind": args.vnc}
    if args.display is not None:
        vnc = {**(vnc or {}), "display": args.display}
    if vnc is not None:
        require(isinstance(vnc, dict), "vnc must be an object")
        bind = address(vnc.get("bind"))
        command += ["--vnc", bind]
        resources.append(listener_resource(bind))
        if "display" in vnc:
            require(isinstance(vnc["display"], str), "vnc.display must be WIDTHxHEIGHT")
            command += ["--display", vnc["display"]]

    efi = (path_from(args.efi_vars, Path.cwd()) if args.efi_vars else
           path_from(config["efi_vars"], base) if "efi_vars" in config else
           str(state_dir / "efi-vars.fd"))
    command += ["--efi-vars", efi, "--serial-log", str(state_dir / "console.log")]
    resources.append("efi:" + efi)
    require(len(resources) == len(set(resources)), "configuration repeats a VM resource")

    shutdown = config.get("shutdown")
    if shutdown is not None:
        require(isinstance(shutdown, dict), "shutdown must be an object")
        host = shutdown.get("host", "127.0.0.1")
        port = shutdown.get("port", ssh_port or 22)
        user = shutdown.get("user", "root")
        require(isinstance(host, str) and host and isinstance(user, str) and user,
                "shutdown host/user must be nonempty")
        require(type(port) is int and 0 < port < 65536, "invalid shutdown port")
        identity = path_from(shutdown.get("identity_file"), base)
        require(Path(identity).is_file(), f"shutdown SSH key does not exist: {identity}")
        shutdown = {"host": host, "port": port, "user": user,
                    "identity_file": identity}
    return {"name": config["name"], "config": str(config_path),
            "command": command, "resources": resources, "shutdown": shutdown, "shares": shares,
            "sudo": args.sudo, "id": uuid.uuid4().hex}


def query(state_dir, request, timeout=3):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(timeout)
        client.connect(str(control_socket_path(state_dir)))
        client.sendall((json.dumps(request) + "\n").encode())
        data = b""
        while not data.endswith(b"\n"):
            chunk = client.recv(65536)
            if not chunk:
                break
            data += chunk
        return json.loads(data)


def live_status(state_dir):
    try:
        return query(state_dir, {"action": "status"})
    except (OSError, ValueError, json.JSONDecodeError):
        return None


def write_json(path, value):
    pending = path.with_suffix(".next")
    pending.write_text(json.dumps(value, indent=2) + "\n")
    pending.replace(path)


def signal_vm(proc, sig, privileged):
    if proc.poll() is not None:
        return
    if privileged:
        result = subprocess.run(["sudo", "-n", "/bin/kill", "-" + str(sig),
                                 "--", "-" + str(proc.pid)], capture_output=True)
        require(result.returncode == 0, result.stderr.decode(errors="replace"))
    else:
        os.killpg(proc.pid, sig)


def force_stop(proc, privileged, timeout):
    signal_vm(proc, signal.SIGTERM, privileged)
    try:
        proc.wait(timeout=timeout)
        return "SIGTERM"
    except subprocess.TimeoutExpired:
        signal_vm(proc, signal.SIGKILL, privileged)
        proc.wait(timeout=5)
        return "SIGKILL"


def guest_shutdown(proc, spec, state_dir, timeout, force_after):
    cfg = spec["shutdown"]
    require(cfg is not None, "no guest SSH shutdown configured; use stop for a forced host stop")
    ssh = ["ssh", "-F", "/dev/null", "-o", "BatchMode=yes",
           "-o", "ConnectTimeout=5", "-o", "StrictHostKeyChecking=accept-new",
           "-o", "UserKnownHostsFile=" + str(state_dir / "known_hosts"),
           "-i", cfg["identity_file"], "-p", str(cfg["port"]),
           cfg["user"] + "@" + cfg["host"], "/sbin/shutdown -p now"]
    start = time.monotonic()
    result = subprocess.run(ssh, capture_output=True, text=True, timeout=min(15, timeout))
    # SSH may disconnect with 255 while the guest shuts down. VM exit zero is
    # the authoritative evidence of a completed guest poweroff.
    remaining = max(0.01, timeout - (time.monotonic() - start))
    try:
        code = proc.wait(timeout=remaining)
    except subprocess.TimeoutExpired:
        if force_after:
            signal_used = force_stop(proc, spec["sudo"], 10)
            return {"ok": False, "result": "forced-after-timeout", "signal": signal_used}
        return {"ok": False, "error": "guest shutdown timed out; VM is still running",
                "ssh": result.stderr[-500:]}
    log = (state_dir / "vmm.log").read_text(errors="replace")
    graceful = code == 0 and "Guest requested PSCI SYSTEM_OFF" in log
    return {"ok": graceful, "result": "guest-poweroff" if graceful else "guest-exit-error",
            "exit_code": code, "ssh": result.stderr[-500:]}


def serve(state_dir):
    lock = (state_dir / "instance.lock").open("a+b")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        return 1
    spec = json.loads((state_dir / "launch.json").read_text())
    control = control_socket_path(state_dir)
    ensure_control_root(control)
    control.unlink(missing_ok=True)
    server = socket.socket(socket.AF_UNIX)
    server.bind(str(control))
    control.chmod(0o600)
    server.listen(8)
    server.settimeout(0.2)
    env = dict(os.environ, VMM_UART_IN=str(state_dir / "uart"))
    command = (["sudo", "-n", "/usr/bin/env", "VMM_UART_IN=" + env["VMM_UART_IN"],
                *spec["command"]] if spec["sudo"] else spec["command"])
    with (state_dir / "vmm.log").open("wb") as output:
        proc = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=output,
                                stderr=subprocess.STDOUT, env=env, start_new_session=True)
        started = time.time()
        record = {"id": spec["id"], "name": spec["name"], "config": spec["config"],
                  "pid": proc.pid, "manager_pid": os.getpid(), "started": started,
                  "resources": spec["resources"], "control_socket": str(control),
                  "shares": spec.get("shares", [])}
        write_json(state_dir / "instance.json", record)
        try:
            while proc.poll() is None:
                try:
                    conn, _ = server.accept()
                except socket.timeout:
                    continue
                with conn:
                    conn.settimeout(3)
                    try:
                        request = json.loads(conn.recv(4096).decode())
                        action = request.get("action")
                        if action == "status":
                            response = {"ok": True, "state": "running", **record}
                        elif action == "stop":
                            signal_used = force_stop(proc, spec["sudo"], request.get("timeout", 10))
                            response = {"ok": True, "result": "forced-stop", "signal": signal_used}
                        elif action == "shutdown":
                            response = guest_shutdown(proc, spec, state_dir,
                                request.get("timeout", 120), request.get("force_after", False))
                        else:
                            response = {"ok": False, "error": "unknown action"}
                    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
                        response = {"ok": False, "error": str(error)}
                    conn.sendall((json.dumps(response) + "\n").encode())
        finally:
            code = proc.wait()
            write_json(state_dir / "last.json", {**record, "state": "stopped",
                                                 "exit_code": code, "ended": time.time()})
            server.close()
            control.unlink(missing_ok=True)
    return 0


def active_resources(root, skip):
    used = set()
    for child in root.iterdir():
        if child == skip or not child.is_dir():
            continue
        status = live_status(child)
        if status and status.get("state") == "running":
            used.update(status["resources"])
    return used


def start(config_path, state_root, state_dir, args):
    state_root.mkdir(mode=0o700, parents=True, exist_ok=True)
    state_dir.mkdir(mode=0o700, exist_ok=True)
    require(not state_root.is_symlink() and not state_dir.is_symlink(),
            "state directories cannot be symlinks")
    with (state_root / ".start.lock").open("a+b") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        require(live_status(state_dir) is None, "this VM is already running")
        spec = launch_spec(config_path, state_dir, args)
        collisions = set(spec["resources"]) & active_resources(state_root, state_dir)
        require(not collisions, "resource already used by a running VM: " + ", ".join(sorted(collisions)))
        if args.sudo:
            require(subprocess.run(["sudo", "-n", "true"], capture_output=True).returncode == 0,
                    "sudo -n is unavailable; run sudo -v first")
        write_json(state_dir / "launch.json", spec)
        with (state_dir / "manager.log").open("ab") as log, open(os.devnull, "rb") as null:
            manager_pid = os.posix_spawn(sys.executable,
                [sys.executable, "-B", __file__, "_serve", str(state_dir)], os.environ,
                file_actions=[(os.POSIX_SPAWN_DUP2, null.fileno(), 0),
                              (os.POSIX_SPAWN_DUP2, log.fileno(), 1),
                              (os.POSIX_SPAWN_DUP2, log.fileno(), 2)], setsid=True)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            status = live_status(state_dir)
            if status:
                require(status["id"] == spec["id"], "unexpected instance identity")
                return status
            last = state_dir / "last.json"
            if last.exists():
                ended = json.loads(last.read_text())
                if ended.get("id") == spec["id"]:
                    details = (state_dir / "vmm.log").read_text(errors="replace")[-2000:]
                    raise RuntimeError(f"VM exited during startup (code {ended['exit_code']}):\n{details}")
            if os.waitpid(manager_pid, os.WNOHANG)[0]:
                details = (state_dir / "manager.log").read_text(errors="replace")[-2000:]
                raise RuntimeError("VM manager exited during startup:\n" + details)
            time.sleep(0.05)
        raise RuntimeError("VM did not start; see " + str(state_dir / "manager.log"))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("start", "status", "shutdown", "stop", "_serve"))
    parser.add_argument("config", type=Path)
    parser.add_argument("--state-root", type=Path,
                        default=Path.home() / ".local/state/vmm")
    parser.add_argument("--binary", help="replace config binary path; relative to current directory")
    parser.add_argument("--ram", type=int, help="replace configured RAM in MiB")
    parser.add_argument("--cpus", type=int, help="replace configured vCPU count")
    parser.add_argument("--net", help="replace network.mode")
    parser.add_argument("--mac", help="replace network.mac")
    parser.add_argument("--ssh-port", type=int, help="replace network.ssh_port")
    parser.add_argument("--forward", action="append", help="replace all configured forwards")
    parser.add_argument("--vnc", help="replace vnc.bind")
    parser.add_argument("--display", help="replace vnc.display")
    parser.add_argument("--efi-vars", help="replace EFI store path")
    parser.add_argument("--disk", action="append", help="replace all disks with these paths")
    parser.add_argument("--share", nargs=2, action="append", metavar=("TAG", "PATH"),
                        help="replace configured shares; export read-only")
    parser.add_argument("--share-rw", nargs=2, action="append", metavar=("TAG", "PATH"),
                        help="replace configured shares; export read-write")
    parser.add_argument("--sudo", action="store_true", help="run the VMM via sudo -n")
    parser.add_argument("--timeout", type=int,
                        help="seconds to wait for guest shutdown (or host stop)")
    parser.add_argument("--force-after-timeout", action="store_true",
                        help="force a host stop if guest shutdown times out")
    args = parser.parse_args(argv)
    if args.sudo and os.geteuid() != 0:
        # Authenticate while still attached to the caller's terminal. A
        # detached manager cannot use a tty-scoped sudo credential cache.
        original = sys.argv[1:] if argv is None else list(argv)
        elevated = [item for item in original if item != "--sudo"]
        os.execvp("sudo", ["sudo", "-n", sys.executable, "-B", __file__,
                           *elevated, "--state-root", str(args.state_root)])
    args.sudo = False
    if args.action == "_serve":
        return serve(args.config)
    config_path = args.config.expanduser().resolve()
    if args.action == "start" or config_path.is_file():
        require(config_path.is_file(), f"configuration does not exist: {config_path}")
        root, state_dir = state_paths(config_path, args.state_root)
    else:
        name = str(args.config)
        require(NAME.fullmatch(name), "use a configuration path or bare VM name")
        root = args.state_root.expanduser().resolve()
        state_dir = root / name
    if args.action == "start":
        result = start(config_path, root, state_dir, args)
    elif args.action == "status":
        result = live_status(state_dir)
        if result is None:
            last = state_dir / "last.json"
            result = json.loads(last.read_text()) if last.exists() else {"state": "stopped"}
    else:
        timeout = args.timeout if args.timeout is not None else (120 if args.action == "shutdown" else 10)
        require(0 < timeout <= 3600, "timeout must be 1–3600 seconds")
        result = query(state_dir, {"action": args.action, "timeout": timeout,
                                   "force_after": args.force_after_timeout},
                       timeout=timeout + 25)
    print(json.dumps(result, indent=2))
    return 0 if result.get("ok", result.get("state") == "stopped") else 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, RuntimeError, json.JSONDecodeError) as error:
        print(f"vmmctl: {error}", file=sys.stderr)
        sys.exit(1)
