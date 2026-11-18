//! Multiboot2 boot header and boot-information memory-map parser (`R13.1`).

pub const MULTIBOOT2_MAGIC: u32 = 0xE852_50D6;
pub const MULTIBOOT2_ARCH_I386: u32 = 0;

/// 24-byte Multiboot2 header + end tag (placed in `.multiboot2`).
#[repr(C, align(8))]
pub struct Multiboot2Header {
    pub magic: u32,
    pub architecture: u32,
    pub header_length: u32,
    pub checksum: u32,
    pub end_tag_type: u16,
    pub end_tag_flags: u16,
    pub end_tag_size: u32,
}

impl Multiboot2Header {
    pub const fn new() -> Self {
        let header_length = core::mem::size_of::<Self>() as u32;
        let checksum = 0u32
            .wrapping_sub(MULTIBOOT2_MAGIC)
            .wrapping_sub(MULTIBOOT2_ARCH_I386)
            .wrapping_sub(header_length);
        Self {
            magic: MULTIBOOT2_MAGIC,
            architecture: MULTIBOOT2_ARCH_I386,
            header_length,
            checksum,
            end_tag_type: 0,
            end_tag_flags: 0,
            end_tag_size: 8,
        }
    }

    pub const fn is_valid(&self) -> bool {
        self.magic == MULTIBOOT2_MAGIC
            && self
                .magic
                .wrapping_add(self.architecture)
                .wrapping_add(self.header_length)
                .wrapping_add(self.checksum)
                == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegion {
    pub base: u64,
    pub length: u64,
    pub kind: u32,
}

/// Parse a Multiboot2 memory-map tag payload (`tag type = 6`).
/// Returns the number of regions written into `out`.
pub fn parse_mmap_entries(tag_payload: &[u8], out: &mut [MemoryRegion]) -> Result<usize, &'static str> {
    if tag_payload.len() < 8 {
        return Err("multiboot2: truncated mmap tag");
    }
    let entry_size = u32::from_le_bytes([
        tag_payload[0],
        tag_payload[1],
        tag_payload[2],
        tag_payload[3],
    ]) as usize;
    if entry_size < 24 {
        return Err("multiboot2: invalid mmap entry_size < 24");
    }
    let mut off = 8usize;
    let mut count = 0usize;
    while off + entry_size <= tag_payload.len() && count < out.len() {
        let base = u64::from_le_bytes(tag_payload[off..off + 8].try_into().unwrap());
        let length = u64::from_le_bytes(tag_payload[off + 8..off + 16].try_into().unwrap());
        let kind = u32::from_le_bytes(tag_payload[off + 16..off + 20].try_into().unwrap());
        out[count] = MemoryRegion { base, length, kind };
        count += 1;
        off += entry_size;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiboot2_header_checksum_sums_to_zero() {
        let hdr = Multiboot2Header::new();
        assert!(hdr.is_valid());
    }

    #[test]
    fn parse_mmap_extracts_regions() {
        let mut buf = [0u8; 8 + 24];
        buf[0..4].copy_from_slice(&24u32.to_le_bytes());
        buf[8..16].copy_from_slice(&0x10_0000u64.to_le_bytes());
        buf[16..24].copy_from_slice(&0x20_0000u64.to_le_bytes());
        buf[24..28].copy_from_slice(&1u32.to_le_bytes());
        let mut out = [MemoryRegion {
            base: 0,
            length: 0,
            kind: 0,
        }; 4];
        let n = parse_mmap_entries(&buf, &mut out).unwrap();
        assert_eq!(n, 1);
        assert_eq!(out[0].base, 0x10_0000);
        assert_eq!(out[0].length, 0x20_0000);
        assert_eq!(out[0].kind, 1);
    }
}
