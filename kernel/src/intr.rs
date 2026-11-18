//! GDT/TSS descriptors, IDT 256-entry table, and exception/interrupt metadata (`R13.2`).

/// 64-bit GDT descriptor entry.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GdtEntry(pub u64);

impl GdtEntry {
    pub const fn null() -> Self {
        Self(0)
    }

    pub const fn kernel_code_64() -> Self {
        // Present | DPL0 | Code/Data | Executable | Readable | Long-mode (L=1)
        Self(0x00AF_9A00_0000_FFFF)
    }

    pub const fn kernel_data_64() -> Self {
        Self(0x00CF_9200_0000_FFFF)
    }

    pub const fn user_code_64() -> Self {
        // DPL=3
        Self(0x00AF_FA00_0000_FFFF)
    }

    pub const fn user_data_64() -> Self {
        Self(0x00CF_F200_0000_FFFF)
    }
}

/// 16-byte x86-64 IDT gate descriptor.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdtEntry {
    pub offset_low: u16,
    pub selector: u16,
    pub ist: u8,
    pub type_attr: u8,
    pub offset_mid: u16,
    pub offset_high: u32,
    pub reserved: u32,
}

impl IdtEntry {
    pub const fn missing() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }

    pub const fn new_interrupt(handler: u64, selector: u16) -> Self {
        Self {
            offset_low: handler as u16,
            selector,
            ist: 0,
            type_attr: 0x8E, // Present, DPL=0, 64-bit Interrupt Gate
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

pub const fn exception_name(vector: u8) -> &'static str {
    match vector {
        0 => "#DE Divide-by-Zero",
        6 => "#UD Invalid Opcode",
        8 => "#DF Double Fault",
        13 => "#GP General Protection Fault",
        14 => "#PF Page Fault",
        32 => "IRQ0 Timer",
        33 => "IRQ1 PS/2 Keyboard",
        _ => "Interrupt",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idt_entry_encodes_64bit_handler_split() {
        let e = IdtEntry::new_interrupt(0xFFFF_8000_0010_1234, 0x08);
        let lo = e.offset_low;
        let mid = e.offset_mid;
        let hi = e.offset_high;
        assert_eq!(lo, 0x1234);
        assert_eq!(mid, 0x0010);
        assert_eq!(hi, 0xFFFF_8000);
        assert_eq!(exception_name(0), "#DE Divide-by-Zero");
        assert_eq!(exception_name(14), "#PF Page Fault");
    }
}
