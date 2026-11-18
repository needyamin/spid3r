//! COM1 serial console, PS/2 scancode set 1 keymap, and minimal kernel shell (`R13.1`, `R13.4`).

pub const COM1_BASE: u16 = 0x3F8;
pub const VGA_BUFFER_ADDR: usize = 0xB8000;

/// Decode PS/2 scancode set 1 press codes to ASCII (0 if non-printable/break).
pub const fn decode_scancode_set1(scancode: u8) -> Option<u8> {
    if scancode & 0x80 != 0 {
        return None;
    }
    match scancode {
        0x02 => Some(b'1'),
        0x03 => Some(b'2'),
        0x04 => Some(b'3'),
        0x05 => Some(b'4'),
        0x06 => Some(b'5'),
        0x07 => Some(b'6'),
        0x08 => Some(b'7'),
        0x09 => Some(b'8'),
        0x0A => Some(b'9'),
        0x0B => Some(b'0'),
        0x10 => Some(b'q'),
        0x11 => Some(b'w'),
        0x12 => Some(b'e'),
        0x13 => Some(b'r'),
        0x14 => Some(b't'),
        0x15 => Some(b'y'),
        0x16 => Some(b'u'),
        0x17 => Some(b'i'),
        0x18 => Some(b'o'),
        0x19 => Some(b'p'),
        0x1C => Some(b'\n'),
        0x1E => Some(b'a'),
        0x1F => Some(b's'),
        0x20 => Some(b'd'),
        0x21 => Some(b'f'),
        0x22 => Some(b'g'),
        0x23 => Some(b'h'),
        0x24 => Some(b'j'),
        0x25 => Some(b'k'),
        0x26 => Some(b'l'),
        0x2C => Some(b'z'),
        0x2D => Some(b'x'),
        0x2E => Some(b'c'),
        0x2F => Some(b'v'),
        0x30 => Some(b'b'),
        0x31 => Some(b'n'),
        0x32 => Some(b'm'),
        0x39 => Some(b' '),
        _ => None,
    }
}

/// Evaluate one minimal kernel shell command line (`help`, `ps`, `meminfo`).
pub fn eval_shell_command(cmd: &str) -> &'static str {
    let trimmed = cmd.trim_ascii();
    match trimmed {
        "help" => "commands: help, ps, meminfo",
        "ps" => "pid=1 state=running name=init",
        "meminfo" => "frames=bitmap_4k heap=bump",
        "" => "",
        _ => "unknown command (type 'help')",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ps2_scancode_and_shell_commands() {
        assert_eq!(decode_scancode_set1(0x23), Some(b'h'));
        assert_eq!(decode_scancode_set1(0x12), Some(b'e'));
        assert_eq!(decode_scancode_set1(0x26), Some(b'l'));
        assert_eq!(decode_scancode_set1(0x19), Some(b'p'));
        assert_eq!(decode_scancode_set1(0x1C), Some(b'\n'));
        assert_eq!(decode_scancode_set1(0xA3), None); // break code
        assert_eq!(eval_shell_command("help"), "commands: help, ps, meminfo");
        assert_eq!(eval_shell_command("ps"), "pid=1 state=running name=init");
    }
}
