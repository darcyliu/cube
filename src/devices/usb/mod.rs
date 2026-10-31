//! PCI xHCI host with a virtual HID keyboard and absolute tablet.
//!
//! BAR0 is [`xhci::MMIO_XHCI_SIZE`] bytes of the existing register file.
//! There is no platform window at `0x0A010000` (that GPA is RAM here).
//! INTx is GSI 24 and is not shared: the I/O APIC stores one level per pin.
//! FreeBSD has no virtio-input driver; `ukbd` / `ums` / `usbhid` use this path.

pub mod hid;
pub mod xhci;
