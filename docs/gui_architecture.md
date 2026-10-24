# GUI Display & Input Architecture Guide

This document outlines the architectural designs, device specifications, host presentation backends, and implementation roadmap for adding Graphical User Interface (GUI) and interactive pointer/keyboard support to the macOS ARM64 `vmm`.

---

## 1. System Architecture Overview

Running a graphical desktop (such as Debian GNOME/XFCE, Alpine desktop, or FreeBSD X11/Wayland) requires three interoperating layers:

```
+-------------------------------------------------------------------------+
| Guest Operating System (Linux / FreeBSD ARM64)                          |
|                                                                         |
|  Desktop Environment (X11 / Wayland / cdebconf GUI)                    |
|      ^                                                |                 |
|      | Mouse / Key events                             v Rendered Pixels |
|  [evdev / Linux Input]                      [DRM / KMS / Framebuffer]   |
+------+------------------------------------------------+-----------------+
       ^                                                |
       | VirtIO MMIO (IRQ 38)                           v GPA Memory / MMIO
+------v------------------------------------------------v-----------------+
| VMM Virtual Hardware (MMIO & RAM)                                       |
|                                                                         |
|  1. Input Device: VirtIO-Input (Tablet/Keyboard)                        |
|  2. Display Adapter:                                                    |
|     - Option A: Simple Framebuffer (`simple-framebuffer` via FDT)       |
|     - Option B: VirtIO-GPU 2D MMIO Device (Device ID 16)                |
+------+------------------------------------------------+-----------------+
       ^                                                |
       | Input Event Queue                              v Framebuffer Buffer
+------v------------------------------------------------v-----------------+
| Host Presentation Backend (macOS Host)                                  |
|                                                                         |
|  [Option 1: Built-in RFB/VNC Server] -> TigerVNC Viewer                 |
|  [Option 2: Native Cocoa Window]     -> NSWindow / MetalKit (CAMetal)   |
|  [Option 3: Web Canvas]              -> HTTP / WebSocket HTML5          |
+-------------------------------------------------------------------------+
```

---

## 2. Guest Display Device Options

### Comparison Matrix

| Feature | Option A: `simple-framebuffer` (FDT) | Option B: `VirtIO-GPU` 2D (MMIO) |
| :--- | :--- | :--- |
| **Complexity** | Minimal (~100 lines of code) | Moderate (~800 lines of code) |
| **Guest Drivers** | Built-in (`simpledrm`, `simplefb`, `vt_simplefb`) | FreeBSD 14 base `virtio_gpu` 2D VT driver; Linux DRM driver |
| **Memory Model** | Reserved GPA physical RAM block | Guest-allocated scatter-gather buffers |
| **Resolution** | Fixed at boot (e.g. 1920x1080 or 1280x720) | Dynamic runtime resizing & multi-head |
| **Hardware 3D** | No (CPU blit only) | Not provided by the FreeBSD base driver |
| **Best For** | Fast bring-up, OS installers, bootloaders | Production desktop VMs, multi-resolution |

---

### Option A: Device Tree `simple-framebuffer` (Recommended Phase 1)

Modern Linux kernels (`CONFIG_DRM_SIMPLEDRM`) and FreeBSD (`vt_simplefb`) natively bind to a standard Device Tree framebuffer node.

#### 1. FDT Specification (`src/fdt.rs`)
In the Device Tree Blob, allocate a memory region at GPA `0x50000000`. The
VMM keeps an 8 MiB minimum reservation and grows it to a page-aligned size for
larger `--display WIDTHxHEIGHT` configurations:

```dts
chosen {
    #address-cells = <2>;
    #size-cells = <2>;

    framebuffer0: framebuffer@50000000 {
        compatible = "simple-framebuffer";
        reg = <0x00 0x50000000 0x00 0x00800000>; /* 8 MiB at 1280x720 */
        width = <1280>;
        height = <720>;
        stride = <(1280 * 4)>;
        format = "a8r8g8b8";
        status = "okay";
    };
};
```

#### 2. How It Works
1. VMM validates width, height, 32-bit stride and total bytes, then reserves at
   least 8 MiB of physical memory at GPA `0x5000_0000`.
2. Guest kernel initializes `/dev/fb0` or direct DRM/KMS device mapped directly to `0x5000_0000`.
3. Guest X11, Wayland, or Debian graphical installer writes BGRA/RGBA pixels directly into this memory region.
4. The VNC server reads the framebuffer when a client requests an update,
   compares incremental regions by row, and returns raw changed rows.

---

### Option B: `VirtIO-GPU` 2D MMIO Device (Recommended Phase 3)

VirtIO-GPU provides standard virtualized 2D display hardware under the VirtIO MMIO v2 specification.

#### 1. Hardware Definition
* **Base Address**: `0x0a000400`
* **MMIO Size**: `0x200`
* **GIC IRQ**: `37` (SPI 5)
* **Device ID**: `16` (`VIRTIO_ID_GPU`)
* **Virtqueues**:
  * Queue 0: `controlq` (Processes 2D graphics requests)
  * Queue 1: `cursorq` (Low-latency mouse cursor updates)

#### 2. Key 2D Commands
```rust
pub const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32        = 0x0100;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32       = 0x0101;
pub const VIRTIO_GPU_CMD_RESOURCE_UNREF: u32           = 0x0102;
pub const VIRTIO_GPU_CMD_SET_SCANOUT: u32              = 0x0103;
pub const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32           = 0x0104;
pub const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32      = 0x0105;
```

#### 3. Execution Lifecycle
1. **Probe**: Guest kernel reads `VIRTIO_GPU_CMD_GET_DISPLAY_INFO` to detect supported scanouts and resolutions.
2. **Allocation**: Guest requests `RESOURCE_CREATE_2D` with width, height, and format (`B8G8R8A8_UNORM`).
3. **Backing Store**: Guest binds guest physical pages using `RESOURCE_ATTACH_BACKING`.
4. **Drawing & Flush**: When the guest finishes drawing a frame, it emits `TRANSFER_TO_HOST_2D` followed by `RESOURCE_FLUSH`. The VMM updates its host display surface.

---

## 3. Guest Input Subsystem (`VirtIO-Input`)

A serial console cannot provide mouse coordinates or graphical keyboard events. We implement standard **`VirtIO-Input`** (`virtio-input-device`, Device ID 18).

### 1. Hardware Definition
* **Base Address**: `0x0a000600` (Tablet), `0x0a000800` (Keyboard)
* **GIC IRQ**: `38`, `39`
* **Device ID**: `18` (`VIRTIO_ID_INPUT`)
* **Virtqueues**:
  * Queue 0: `eventq` (Host-to-guest input events)
  * Queue 1: `statusq` (Guest-to-host feedback, e.g. CapsLock LED)

### 2. Linux Event Format (16-Byte `virtio_input_event`)
```rust
#[repr(C)]
pub struct VirtioInputEvent {
    pub event_type: u16, // EV_KEY (0x01), EV_REL (0x02), EV_ABS (0x03), EV_SYN (0x00)
    pub code: u16,       // Key scancode (KEY_A) or axis (ABS_X, ABS_Y, BTN_LEFT)
    pub value: u32,      // 1=down, 0=up, or coordinate (0..32767)
}
```

### 3. Absolute Pointer (Tablet) vs. Relative Mouse
* **Relative Mouse (`EV_REL`)**: Requires host mouse grabbing/trapping (cursor lock). If pointer acceleration differs between host and guest, the guest cursor drifts away from the host cursor.
* **Absolute Tablet (`EV_ABS`)**: Sends absolute `(X, Y)` normalized coordinates `(0..32767)` mapped to screen width and height. The guest cursor precisely tracks the host mouse pointer without requiring cursor grabbing.

---

## 4. Host Presentation Backend Options

| Backend | Implementation Details | Pros | Cons |
| :--- | :--- | :--- | :--- |
| **1. Embedded RFB/VNC Server** *(Recommended)* | Lightweight TCP server on `localhost:5900` implementing RFC 6143 (RFB 3.8). | Works headless/SSH with a standard client such as TigerVNC. | Requires a compatible VNC viewer; macOS Screen Sharing requires password authentication. |
| **2. Native Cocoa / Metal Window** *(first pass implemented)* | AppKit window with an `MTKView` presenting the existing guest framebuffer. | Local desktop in one window; no VNC viewer or RFB encoding. | Fixed guest resolution and US physical key mapping; full desktop input validation and packaging remain. |
| **3. WebSocket / HTML5 Canvas** | Mini HTTP server + WebSocket streaming raw RGBA / PNG frames to a browser. | Works in any browser without client software. | Requires JavaScript frontend. |

---

### Native Cocoa / Metal window: implemented first pass

This is a **host presentation option**, not a new guest display device. The
current `--vnc` path creates the FDT simple-framebuffer, platform xHCI HID
keyboard/tablet, and (for Linux) virtio-input devices in `src/main.rs`. The
`--gui native` path reuses those guest devices and replaces the RFB frontend
with a local AppKit window. It does **not** require virtio-gpu,
change the guest driver, or provide guest 3D acceleration. The current VNC
frontend remains useful for headless runs and automated checks.

Proposed data flow:

```text
guest X11/desktop -> simple-framebuffer in guest RAM -> host snapshot/staging
                  -> Metal texture -> MTKView in an NSWindow
AppKit mouse/key/scroll events -> shared input translation -> xHCI HID
                                                     (and virtio-input for Linux)
```

- **Window and VM lifetime:** AppKit owns the main-thread application/window
  event loop; the VM execution and blocking I/O run on worker threads. Closing
  the window needs an explicit policy (hide, request guest shutdown, or stop
  the VM). VM reset must keep the window and input devices in sync.
- **Pixels:** start with the existing fixed-size BGRA simple-framebuffer.
  Snapshot changed pixels into staging memory, upload them to a Metal texture,
  and draw that texture in an `MTKView`. Preserve the guest aspect ratio and
  map between guest pixels and Retina/backing pixels. Cap refresh work when
  the guest is idle or the window is occluded. Metal speeds host presentation;
  guest rendering still uses the unaccelerated framebuffer.
- **Input:** translate AppKit key events and modifier changes into the same
  guest key/button state used by VNC. Convert view coordinates to absolute
  tablet coordinates after accounting for scaling and letterboxing. Handle
  wheel direction, key repeat, focus loss, and window close; release held
  keys/buttons when focus is lost so the guest cannot retain a stuck input.
- **Code boundary:** extract framebuffer access and guest input injection from
  `src/devices/display/vnc.rs` into reusable interfaces. Keep RFB protocol
  handling in the VNC frontend; put AppKit/Metal interop behind a macOS-only
  frontend. A small Swift/Objective-C bridge or Rust bindings are both possible;
  choose after a minimal window-and-texture prototype establishes the build
  and signing path.
- **Validation:** retain the VNC regression for device-level behavior, then
  add native-window checks for rendering, pointer edges, clicks/wheel,
  modifiers, focus-loss release, resize/scaling, reboot, and clean shutdown.
  Measure capture/upload/present time and input-to-frame latency before
  claiming a performance benefit.

The first pass is integrated into the CLI as `--gui native`. The AppKit/MetalKit
frontend lives in `src/native.m`, with the framebuffer and HID bridge in
`src/native.rs`. AppKit runs on the main thread while VM setup and vCPU execution
run on a worker. It copies the BGRA framebuffer into a Metal texture at up to
30 frames/s, scales it to an integer number of Retina drawable pixels with
nearest-pixel sampling and letterboxing, and maps mouse, wheel, and US-layout
physical keyboard events to the existing xHCI devices. Focus loss releases
held guest input; closing the window requests VM shutdown. VNC remains a
separate frontend and cannot be selected simultaneously. A guest reboot still
uses the existing process re-exec path, so the native window closes and reopens.
The optional `--retina` flag doubles the framebuffer dimensions relative to
`--display` while keeping the initial Cocoa window size in points; guest-side
DPI scaling is still needed for readable desktop controls at that resolution.
Packaging, keyboard-layout/accessibility polish, and a full interactive XFCE
regression remain to do. Runtime guest resolution changes remain a separate
virtio-gpu or display-device project.
Apple documents [`MTKView`](https://developer.apple.com/documentation/metalkit/mtkview/)
as a Metal-aware view and [`NSApplication`](https://developer.apple.com/documentation/appkit/nsapplication)
as the owner of the application event loop.

---

## 5. Recommended Implementation Roadmap

### Phase 1: SimpleFB + Embedded VNC Server
1. **Memory Allocation**: Reserve 8MB RAM at `0x5000_0000` for 1920x1080 32bpp framebuffer.
2. **DTB Entry**: Generate `framebuffer` node in `src/fdt.rs`.
3. **VNC Server (`src/devices/display/vnc.rs`)**:
   - Implement minimal RFB protocol (`ProtocolVersion`, `SecurityHandshake`, `SetPixelFormat`, `FramebufferUpdateRequest`).
   - Listen on `127.0.0.1:5900`.
4. **Validation**: Run `make debian` or `make alpine`, install TigerVNC with `brew install --cask tigervnc`, and connect with `/Applications/TigerVNC.app/Contents/MacOS/vncviewer 127.0.0.1::5900`.

### Phase 2: Interactive Pointer & Keyboard via VirtIO-Input
1. Implement `VirtioInput` MMIO device for Tablet (`EV_ABS`) and Keyboard (`EV_KEY`).
2. Map incoming VNC `PointerEvent` and `KeyEvent` packets to `VirtioInputEvent` descriptors.
3. Assert GIC IRQ 38/39 and kick vCPUs.

### Phase 3: Optional native Cocoa/Metal frontend
1. [x] Expose guest framebuffer and xHCI HID input to a separate local frontend.
2. [x] Add the AppKit/Metal window and a `--gui native` option using simple-framebuffer.
3. [ ] Verify interactive XFCE input, lifecycle, and performance against the VNC baseline.

### Separate future track: VirtIO-GPU 2D
Implement the virtio-gpu control/cursor queues only when a guest capability or
measured display limitation justifies a new virtual display device. It is not
a prerequisite for the native window.


## FreeBSD input reality check (2026-09-20)

Stock FreeBSD (main / stable/14 / releng/14.3) defines `VIRTIO_ID_INPUT` (18) in
`virtio_ids.h` and names it in the virtio device table, but **ships no
`virtio_input` driver**. A FreeBSD guest will therefore only set status
`ACKNOWLEDGE` (`0x1`) on our virtio-input MMIO devices and never reach
`DRIVER_OK` — host inject queues grow forever.

Practical options for FreeBSD GUI input:
1. **USB HID keyboard + absolute tablet** (what QEMU/UTM use for FreeBSD) — needs xHCI + HID gadget/emulation in the VMM.
2. **Validate virtio-input on a Linux ARM64 guest** first (driver exists there), keep FreeBSD on serial until USB lands.
3. **Port/write a FreeBSD `virtio_input` + evdev driver** — large guest-side project.

Host virtio-input + VNC mapping can stay; it is not usable under stock FreeBSD until (1) or (3).


## FreeBSD USB HID input (platform xHCI)

Stock FreeBSD has **no** `virtio_input` driver (devices stay at status `ACKNOWLEDGE`).
When `--vnc` is set, the VMM also exposes a platform MMIO **xHCI** controller:

| Item | Value |
|------|-------|
| FDT compatible | `generic-xhci` (FreeBSD `sys/dev/usb/controller/generic_xhci_fdt.c` on simplebus) |
| MMIO base | `0x0a010000` (64 KiB) |
| GIC IRQ | `41` (SPI 9) |
| Root ports | 2 × USB2 High-Speed |
| Port 1 | HID boot keyboard → FreeBSD `ukbd`, or `usbhid` / `hkbd` |
| Port 2 | Absolute tablet (QEMU-compatible report desc, X/Y 0..0x7fff) → `usbhid` / `hms` |

VNC KeyEvent/PointerEvent prefer USB HID inject; virtio-input remains registered for Linux guests but is not required for FreeBSD.

### xHCI subset implemented

- Capability / operational / runtime / doorbell register blocks (QEMU-like layout)
- USBCMD Run/Stop + HCRST; PORTSC connect+reset for 2 HS ports
- Command ring: Enable/Disable Slot, Address Device, Configure/Evaluate Endpoint,
  Reset/Stop Endpoint, Set TR Dequeue, Reset Device, No-Op
- Control transfers for device/config/string/HID report descriptors + HID class reqs
- Interrupt IN Normal TRBs completed when a HID report is queued (else deferred)
- Event ring + GIC SPI interrupt

**Gaps:** no isoch/bulk/streams; HS only; simplified device-context templates; if
enumeration stalls, check host `[XHCI]` logs and guest `dmesg` / `usbconfig`.

### Guest verification (FreeBSD)

```bash
sysctl hw.usb.usbhid.enable=1 kern.evdev.rcpt_mask=6
kldload hms
usbconfig dump_device_desc
# Find iProduct <VMM USB Tablet> and iSerialNumber <tablet0>.
# Reset that descriptor's ugenN.N device, for example:
usbconfig -d ugen0.3 reset
dmesg | grep -iE 'xhci|hkbd|hms|hidbus|usbhid'
usbconfig
# type on the VNC console — characters should appear on ttyv0 / login
```

FreeBSD 14.4 leaves `usbhid` disabled by default. Direct kernel boot bypasses
`/boot/loader.conf`, so enable it at runtime and reset the tablet. Otherwise,
the absolute tablet attaches to generic `uhid`; legacy `ums` accepts relative
mouse axes and cannot expose this device to an X11 desktop.

Rebuild/run:

```bash
CARGO_TARGET_DIR=/tmp/cube-target cargo build
./target/debug/cube --freebsd --vnc 127.0.0.1:5901
# or with CARGO_TARGET_DIR: /tmp/cube-target/debug/cube ...
```

## Verified fixes and current scope

The first implementation had several independent failures:

- xHCI Evaluate Context reused Configure Endpoint handling and rewound EP0 to
  its original dequeue address. FreeBSD evaluates EP0 after reading descriptors;
  replaying old transfers caused enumeration timeouts and `Root mount waiting
  for: usbus0`. Evaluation now updates the max packet size without resetting
  the live ring or slot state.
- USB address and slot state were written into the wrong slot-context dword.
  Port-change events also placed the port ID in the control field instead of
  the parameter field. These layouts now match the guest driver.
- Standard interface GET_DESCRIPTOR returned STALL before reaching the HID
  report-descriptor handler. Keyboard and tablet report descriptors are now
  served from the standard request handler.
- The default FreeBSD boot options forced serial-only console operation. With
  `--vnc`, direct boot now enables multiple consoles (`-D` / `RB_MULTIPLE`),
  allowing both framebuffer output and USB keyboard console input.
- Framebuffer backing was advertised as allocatable RAM. The FDT memory ranges
  now exclude its 8 MiB region while the host mapping remains available.
- RFB SetPixelFormat was ignored, breaking clients requesting a different pixel
  size or byte order. Raw updates now encode the negotiated true-colour format.
  RFB 3.7 security None no longer receives the 3.8-only SecurityResult.
- Repeated interrupt endpoint doorbells could enqueue the same transfer twice.
  Each pending transfer is now tracked once.

### Build and try

```sh
CARGO_TARGET_DIR=/tmp/vmm-gui-fix cargo build
codesign --force --sign - --entitlements entitlements.plist /tmp/vmm-gui-fix/debug/vmm
/tmp/vmm-gui-fix/debug/vmm --freebsd .iso-boot/boot/kernel/kernel \
  --net none --vnc 127.0.0.1:5999
```

Use an explicit existing kernel path; `boot/kernel/kernel` is not present in
this workspace. Add the usual disk and root options to boot userland. With no
attached disk, reaching `mountroot>` is expected and is useful for testing the
console without modifying any disk image.

Validated against the local FreeBSD kernel: `VT(simplefb)` at 1280×720,
`ukbd0` attachment, both USB devices enumerated, HID report descriptor reads,
VNC framebuffer delivery in native 32-bit and RGB565 formats, and VNC typing
`.` plus Enter producing a response at `mountroot>`. Regression tests cover
context evaluation, USB address layout, port events, descriptors, duplicate
pending transfers, pixel encoding, and the generated memory ranges.

This provides the display and input hardware for a graphical desktop. The
diskless check cannot load the userland X11 stack, so complete pointer behavior
is verified separately by [the FreeBSD desktop runbook](freebsd-desktop.md).
UEFI loader console settings remain independent of the direct-kernel defaults.

Each VNC disconnect now logs requests, bytes scanned, pixels and wire bytes
sent, and time spent reading guest RAM, comparing rows, and writing updates.
Use those server figures together with the desktop verifier's client timing
before changing the update algorithm.

### Host diagnostics

Routine xHCI transfers, VNC key/handshake events, and DHCP DNS rewrites are quiet
by default. Set `VMM_DEVICE_TRACE=1` before launching the VM to enable these
traces for debugging. Errors and VNC connection messages remain visible; their
line endings account for the serial console's raw terminal mode. Redirected
logs use normal LF endings.
