//! PCI configuration space & virtio (`virtio-net`, `virtio-blk`) descriptors (`R13.7`).

pub const PCI_VENDOR_VIRTIO: u16 = 0x1AF4;
pub const VIRTIO_DEV_NET_MODERN: u16 = 0x1041;
pub const VIRTIO_DEV_BLK_MODERN: u16 = 0x1042;

/// Encode a 32-bit PCI Type-0 configuration space address for I/O port `0xCF8`.
pub const fn pci_config_addr(bus: u8, slot: u8, func: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | (((slot as u32) & 0x1F) << 11)
        | (((func as u32) & 0x07) << 8)
        | ((offset as u32) & 0xFC)
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtqDesc {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
    pub next: u16,
}

pub const fn classify_virtio_device(vendor: u16, device: u16) -> Option<&'static str> {
    if vendor != PCI_VENDOR_VIRTIO {
        return None;
    }
    match device {
        0x1000 | VIRTIO_DEV_NET_MODERN => Some("virtio-net"),
        0x1001 | VIRTIO_DEV_BLK_MODERN => Some("virtio-blk"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pci_addr_and_virtio_classification() {
        assert_eq!(pci_config_addr(0, 3, 0, 0), 0x8000_1800);
        assert_eq!(
            classify_virtio_device(PCI_VENDOR_VIRTIO, VIRTIO_DEV_NET_MODERN),
            Some("virtio-net")
        );
        assert_eq!(
            classify_virtio_device(PCI_VENDOR_VIRTIO, VIRTIO_DEV_BLK_MODERN),
            Some("virtio-blk")
        );
        assert_eq!(classify_virtio_device(0x8086, 0x1234), None);
    }
}
