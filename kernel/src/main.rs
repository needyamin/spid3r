//! Spid3rOS bare-metal kernel entry (`R13.1`).
//!
//! Under `target_os = "none"`, exports `_start`, embeds the `.multiboot2`
//! header, writes the boot banner over COM1 (`0x3F8`), and halts cleanly.

#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

#[cfg(target_os = "none")]
use core::arch::asm;

#[cfg(target_os = "none")]
#[link_section = ".multiboot2"]
#[used]
static MULTIBOOT2_HDR: spid3r_kernel::boot::Multiboot2Header =
    spid3r_kernel::boot::Multiboot2Header::new();

#[cfg(target_os = "none")]
#[inline(always)]
unsafe fn outb(port: u16, val: u8) {
    // SAFETY: Caller only writes to documented COM1 serial ports (`0x3F8..0x3FD`).
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

#[cfg(target_os = "none")]
#[inline(always)]
unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    // SAFETY: Caller only reads documented COM1 LSR port (`0x3FD`).
    unsafe {
        asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    v
}

#[cfg(target_os = "none")]
fn serial_init() {
    let base = spid3r_kernel::console::COM1_BASE;
    // SAFETY: Standard 16550 UART initialization on COM1 (`0x3F8`).
    unsafe {
        outb(base + 1, 0x00);
        outb(base + 3, 0x80);
        outb(base + 0, 0x03);
        outb(base + 1, 0x00);
        outb(base + 3, 0x03);
        outb(base + 2, 0xC7);
        outb(base + 4, 0x0B);
    }
}

#[cfg(target_os = "none")]
fn serial_write_bytes(bytes: &[u8]) {
    let base = spid3r_kernel::console::COM1_BASE;
    for &b in bytes {
        // SAFETY: Polls COM1 LSR (`base + 5`) and writes to THR (`base`).
        unsafe {
            while (inb(base + 5) & 0x20) == 0 {}
            outb(base, b);
        }
    }
}

#[cfg(target_os = "none")]
#[no_mangle]
pub extern "C" fn _start() -> ! {
    serial_init();
    serial_write_bytes(b"Spid3rOS v0.1 (x86_64 multiboot2) ready\n");
    loop {
        // SAFETY: `hlt` idles the CPU until the next interrupt.
        unsafe {
            asm!("hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(target_os = "none")]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    serial_write_bytes(b"PANIC: Spid3rOS halted\n");
    loop {
        // SAFETY: Halt CPU on kernel panic.
        unsafe {
            asm!("cli; hlt", options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(not(target_os = "none"))]
fn main() {
    println!("Spid3rOS kernel host binary — use `spid3r kernel build` for bare-metal x86_64-unknown-none");
}
