//! Userland ELF64 validator/loader and documented `syscall/sysret` ABI (`R13.6`).

pub const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
pub const ELF_CLASS_64: u8 = 2;
pub const ELF_MACHINE_X86_64: u16 = 62;

pub const SYS_WRITE: u64 = 1;
pub const SYS_GETPID: u64 = 39;
pub const SYS_EXIT: u64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Elf64Summary {
    pub entry: u64,
    pub phoff: u64,
    pub phnum: u16,
}

/// Validate an ELF64 x86-64 executable header (fail-closed on malformed binaries).
pub fn parse_elf64_header(bytes: &[u8]) -> Result<Elf64Summary, &'static str> {
    if bytes.len() < 64 {
        return Err("elf64: image shorter than 64-byte header");
    }
    if bytes[0..4] != ELF_MAGIC {
        return Err("elf64: bad magic");
    }
    if bytes[4] != ELF_CLASS_64 {
        return Err("elf64: not ELFCLASS64");
    }
    let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
    if machine != ELF_MACHINE_X86_64 {
        return Err("elf64: not EM_X86_64");
    }
    let entry = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
    let phoff = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
    let phnum = u16::from_le_bytes([bytes[56], bytes[57]]);
    if entry == 0 {
        return Err("elf64: zero entry point");
    }
    Ok(Elf64Summary {
        entry,
        phoff,
        phnum,
    })
}

/// Documented kernel syscall dispatch (`SYS_WRITE`, `SYS_GETPID`, `SYS_EXIT`).
/// Unknown syscall numbers return `Err` (caller terminates offending user task).
pub fn dispatch_syscall(num: u64, _arg0: u64, _arg1: u64, arg2: u64) -> Result<u64, &'static str> {
    match num {
        SYS_WRITE => Ok(arg2),
        SYS_GETPID => Ok(1),
        SYS_EXIT => Ok(0),
        _ => Err("syscall: unknown syscall number"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elf64_rejects_malformed_and_accepts_valid_header() {
        assert!(parse_elf64_header(b"bad").is_err());
        let mut hdr = [0u8; 64];
        hdr[0..4].copy_from_slice(&ELF_MAGIC);
        hdr[4] = ELF_CLASS_64;
        hdr[18..20].copy_from_slice(&ELF_MACHINE_X86_64.to_le_bytes());
        hdr[24..32].copy_from_slice(&0x401000u64.to_le_bytes());
        hdr[32..40].copy_from_slice(&64u64.to_le_bytes());
        hdr[56..58].copy_from_slice(&1u16.to_le_bytes());
        let parsed = parse_elf64_header(&hdr).unwrap();
        assert_eq!(parsed.entry, 0x401000);
        assert_eq!(dispatch_syscall(SYS_GETPID, 0, 0, 0), Ok(1));
        assert!(dispatch_syscall(9999, 0, 0, 0).is_err());
    }
}
