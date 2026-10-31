//! PCI xHCI host with a virtual HID keyboard and absolute tablet.
//!
//! BAR0 is [`xhci::MMIO_XHCI_SIZE`] bytes of the register file.
//! The INTx pin is one I/O APIC level and is not shared.

pub mod hid;
pub mod xhci;
