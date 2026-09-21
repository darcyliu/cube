//! Platform (FDT) USB host: generic-xhci + virtual HID keyboard/tablet.
//!
//! Stock FreeBSD has no virtio_input driver, but does bind
//! `compatible = "generic-xhci"` on simplebus. This module provides a
//! pragmatic MMIO xHCI controller with two root-port HID devices so VNC
//! KeyEvent/PointerEvent can reach FreeBSD `ukbd` / `ums` (or `usbhid`).

pub mod hid;
pub mod xhci;

