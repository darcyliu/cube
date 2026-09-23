# FreeBSD desktop verification

This runbook validates the VNC display and USB HID path with a real FreeBSD
X11 session. It uses a disposable installed disk because package installation
and Xorg configuration modify the guest.

## Prerequisites

- Apple Silicon macOS and the signed VMM binary.
- A retained UFS result from `scripts/check_install.py --keep`. Its artifact
  directory contains `installed.img`, `boot/kernel/kernel`, and `id_ed25519`.
- Guest network access through vmnet.

The automated regression creates a disposable UFS installation, installs the
desktop packages, starts Xorg with SCFB, runs the input/update verifier, reboots,
and powers off cleanly:

```sh
sudo python3 scripts/check_install.py /tmp/vmm-gui-fix/debug/vmm \
  --iso FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso \
  --desktop --keep
```

Success includes the three desktop `PASS` lines documented below. The retained
artifact directory contains `desktop-results/display-metrics.json`, the XI2 event
and device logs, `desktop-check.log`, the VMM logs, and copies of the Xorg logs.

The 2026-09-22 FreeBSD 14.4 run passed all three desktop checks and the full
install → boot → reboot regression. At 1024×768, 32 framebuffer updates
averaged 38.4 updates/s and 14.0 MiB/s of raw pixel payload. The retained
artifacts are `/private/tmp/vmm-install-w_xfxah8`. The two VNC session records
report 96.0 MiB of framebuffer reads in 2.6 ms, 90.0 MiB of changed-row
comparisons in 3.6 ms, and 11.7 MiB sent in 291.0 ms. The send time was the
largest measured server cost in this run. `guest-hid.log` confirms `hms0`
attached with five buttons and X/Y/wheel axes.

The remaining commands describe the equivalent manual workflow for diagnosis.
Use the printed artifact directory as `$INSTALL`. Start its installed system
with a configurable 1024×768 framebuffer:

```sh
sudo /tmp/vmm-gui-fix/debug/vmm \
  --freebsd "$INSTALL/boot/kernel/kernel" --multiuser \
  --disk "$INSTALL/installed.img" --ram 2048 --cpus 2 \
  --net vmnet --ssh-port 2222 \
  --vnc 127.0.0.1:5901 --display 1024x768 \
  --serial-log "$INSTALL/desktop-vmm.log"
```

In another terminal, install the documented SCFB X11 stack:

```sh
SSH="ssh -F /dev/null -i $INSTALL/id_ed25519 -p 2222 \
  -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new \
  -o UserKnownHostsFile=$INSTALL/desktop-known-hosts root@127.0.0.1"
$SSH 'sysctl hw.usb.usbhid.enable=1 kern.evdev.rcpt_mask=6 && \
  (kldload hms 2>/dev/null || true) && \
  usbconfig dump_device_desc'
```

FreeBSD 14.4 defaults to the legacy USB HID drivers. This VMM boots the UFS
kernel directly, so it does not read `/boot/loader.conf`. Enable `usbhid` at
runtime, then reset the device whose descriptor has `iProduct` equal to
`VMM USB Tablet` and `iSerialNumber` equal to `tablet0`. Use its `ugenN.N`
identifier printed above. The summary names come from a USB ID database and
can label both the keyboard and tablet as `QEMU Tablet`.

```sh
$SSH 'usbconfig -d ugen0.3 reset; sleep 2; dmesg | grep -E "usbhid|hms"'
```

Replace `ugen0.3` with the matching device identifier. The `hms` driver must
attach to expose the tablet's absolute axes through evdev. Then install the
desktop packages:

```sh
$SSH 'env ASSUME_ALWAYS_YES=yes pkg bootstrap && \
  pkg install -y xorg-minimal xf86-video-scfb xf86-input-libinput \
    twm xterm xinput xdpyinfo && \
  sysctl kern.evdev.rcpt_mask=6 && \
  mkdir -p /usr/local/etc/X11/xorg.conf.d && \
  printf "%s\n" "Section \"Device\"" "  Identifier \"Card0\"" \
    "  Driver \"scfb\"" "EndSection" \
    >/usr/local/etc/X11/xorg.conf.d/20-scfb.conf'
```

Start Xorg, the window manager, and one moving/resizable client:

```sh
$SSH 'pkill Xorg 2>/dev/null || true; \
  daemon -f -p /var/run/Xorg-vmm.pid -o /var/log/Xorg-vmm.log \
    /usr/local/bin/Xorg :0 -ac -noreset vt9'
$SSH 'until env DISPLAY=:0 xdpyinfo >/dev/null 2>&1; do sleep 1; done; \
  daemon -f -p /var/run/twm-vmm.pid -o /var/log/twm-vmm.log \
    /usr/bin/env DISPLAY=:0 /usr/local/bin/twm; \
  daemon -f -p /var/run/xterm-vmm.pid -o /var/log/xterm-vmm.log \
    /usr/bin/env DISPLAY=:0 /usr/local/bin/xterm -geometry 90x28+80+80'
```

Run the verifier from the host:

```sh
python3 scripts/check_desktop.py \
  --vnc 127.0.0.1:5901 --ssh-port 2222 \
  --identity "$INSTALL/id_ed25519" \
  --artifacts "$INSTALL/desktop-results"
```

The check uses `xinput --test-xi2 --root` and requires X11 to observe pointer
coordinates at opposite framebuffer edges, primary-button press/release, wheel
directions, keyboard press/release, and synthesized releases after disconnecting
with Control and the primary button held. It reconnects, requests full and
incremental frames, and writes client timing and payload measurements to
`display-metrics.json`, including when an input assertion fails. The VMM log adds
server-side framebuffer-read, changed-row scan, send time, and byte totals for
every VNC session.

Record the script's `PASS` lines and retain `desktop-results`, `desktop-vmm.log`,
`guest-hid.log`, `Xorg.0.log`, and `/var/log/Xorg-vmm.log` when closing the
roadmap criteria.
Stop the guest with `shutdown -p now`; Ctrl-] remains available as a host stop.

## Virtio-gpu decision

FreeBSD 14.4 contains `virtio_gpu`, introduced in FreeBSD 14.0, and ARM64
GENERIC includes it through `std.virt`. The driver creates a 2D `vt(4)`
framebuffer; it does not provide a DRM/VirGL accelerated desktop. The
ArmVirtQemu EDK2 build includes `VirtioGpuDxe`, so a future VMM device can also
provide GOP output before the kernel starts.

The decision for this milestone is **defer**. Implementing the control queue,
guest-backed 2D resources, scanout and cursor queue is feasible, but it adds no
known compatibility requirement for the SCFB desktop and no 3D acceleration.
Keep simple-framebuffer as the supported path, collect the measurements above,
and reconsider virtio-gpu if changed-row scanning or fixed boot-time geometry
becomes the limiting behavior.

Sources: [FreeBSD virtio_gpu(4) in 14.4](https://cgit.freebsd.org/src/tree/share/man/man4/virtio_gpu.4?h=releng/14.4),
[FreeBSD ARM64 std.virt in 14.4](https://cgit.freebsd.org/src/tree/sys/arm64/conf/std.virt?h=releng/14.4),
[FreeBSD X11 handbook](https://docs.freebsd.org/en/books/handbook/x11/), and
[EDK2 ArmVirtQemu configuration](https://github.com/tianocore/edk2/blob/edk2-stable202408/ArmVirtPkg/ArmVirtQemu.dsc).
