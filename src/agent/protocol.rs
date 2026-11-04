//! Wire format for `spid3r-agent/1`.
//!
//! Every connection is a sequence of frames:
//!
//! ```text
//! magic    8 bytes  "SPID3R1"
//! version  u8       1
//! kind     u8       0 pair | 1 data | 2 error
//! device  16 bytes  device id (zero on pair)
//! nonce   12 bytes  AEAD nonce
//! length   u32 BE   payload length
//! payload
//! ```
//!
//! `kind = pair` payloads are plaintext JSON (the pairing code is the secret).
//! `kind = data` payloads are ChaCha20-Poly1305(key, nonce, aad = header, json).
//! The header is bound as AAD, so a replayed header with a swapped payload
//! fails the tag.

use crate::crypto::{self, KEY_LEN, NONCE_LEN};

pub const MAGIC: &[u8; 8] = b"SPID3R01";
pub const VERSION: u8 = 1;
pub const KIND_PAIR: u8 = 0;
pub const KIND_DATA: u8 = 1;
pub const KIND_ERROR: u8 = 2;
pub const HEADER_LEN: usize = 8 + 1 + 1 + 16 + NONCE_LEN + 4;
pub const MAX_PAYLOAD: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Header {
    pub kind: u8,
    pub device: [u8; 16],
    pub nonce: [u8; NONCE_LEN],
    pub len: u32,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[..8].copy_from_slice(MAGIC);
        out[8] = VERSION;
        out[9] = self.kind;
        out[10..26].copy_from_slice(&self.device);
        out[26..38].copy_from_slice(&self.nonce);
        out[38..42].copy_from_slice(&self.len.to_be_bytes());
        out
    }

    pub fn decode(buf: &[u8; HEADER_LEN]) -> std::result::Result<Header, String> {
        if &buf[..8] != MAGIC {
            return Err("bad magic (not a Spid3r agent)".to_string());
        }
        if buf[8] != VERSION {
            return Err(format!("unsupported agent version {}", buf[8]));
        }
        let mut device = [0u8; 16];
        device.copy_from_slice(&buf[10..26]);
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&buf[26..38]);
        Ok(Header {
            kind: buf[9],
            device,
            nonce,
            len: u32::from_be_bytes([buf[38], buf[39], buf[40], buf[41]]),
        })
    }

    pub fn aad(&self) -> [u8; HEADER_LEN] {
        self.encode()
    }
}

/// Write one frame. `payload` is already sealed (ciphertext||tag) for data frames.
pub fn write_frame(
    w: &mut impl std::io::Write,
    kind: u8,
    device: &[u8; 16],
    payload: &[u8],
    key: Option<&[u8; KEY_LEN]>,
) -> std::io::Result<()> {
    let mut nonce = [0u8; NONCE_LEN];
    crypto::random_fill(&mut nonce)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    // The length field is always the number of bytes that follow the header,
    // so for an encrypted frame it is plaintext + TAG_LEN. That is also the
    // header we bind as AAD, or the peer cannot open the frame.
    let wire_len = match key {
        Some(_) => payload.len() + crypto::TAG_LEN,
        None => payload.len(),
    };
    let hdr = Header {
        kind,
        device: *device,
        nonce,
        len: wire_len as u32,
    };
    let aad = hdr.aad();
    let body = match key {
        Some(k) => crypto::aead_seal(k, &nonce, &aad, payload),
        None => payload.to_vec(),
    };
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&hdr.encode());
    out.extend_from_slice(&body);
    w.write_all(&out)?;
    w.flush()
}

/// Read one frame. Returns (header, plaintext_or_ciphertext, key_used).
pub fn read_frame(
    r: &mut impl std::io::Read,
    key_lookup: &dyn Fn(&[u8; 16]) -> Option<[u8; KEY_LEN]>,
) -> Result<(Header, Vec<u8>)> {
    let mut hdr_buf = [0u8; HEADER_LEN];
    r.read_exact(&mut hdr_buf).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            "closed".to_string()
        } else {
            e.to_string()
        }
    })?;
    let hdr = Header::decode(&hdr_buf)?;
    if hdr.len as usize > MAX_PAYLOAD {
        return Err(format!("frame too large: {}", hdr.len));
    }
    let mut body = vec![0u8; hdr.len as usize];
    r.read_exact(&mut body).map_err(|e| e.to_string())?;
    let plaintext = match hdr.kind {
        KIND_PAIR => body,
        KIND_DATA | KIND_ERROR => {
            let key = key_lookup(&hdr.device)
                .ok_or_else(|| "unknown device".to_string())?;
            crypto::aead_open(&key, &hdr.nonce, &hdr.aad(), &body)
                .map_err(|_| "authentication failed".to_string())?
        }
        other => return Err(format!("unknown frame kind {}", other)),
    };
    Ok((hdr, plaintext))
}

pub type Result<T> = std::result::Result<T, String>;

/// Device id as 16 raw bytes from 32 hex chars.
pub fn device_bytes(hex: &str) -> Option<[u8; 16]> {
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

pub fn device_hex(bytes: &[u8; 16]) -> String {
    crate::pkg::hex_encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = Header {
            kind: KIND_DATA,
            device: [1; 16],
            nonce: [2; 12],
            len: 1234,
        };
        let enc = h.encode();
        let back = Header::decode(&enc).unwrap();
        assert_eq!(back.kind, KIND_DATA);
        assert_eq!(back.device, [1; 16]);
        assert_eq!(back.nonce, [2; 12]);
        assert_eq!(back.len, 1234);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = [0u8; HEADER_LEN];
        buf[..8].copy_from_slice(b"NOTMAGIC");
        assert!(Header::decode(&buf).is_err());
    }

    #[test]
    fn device_hex_roundtrip() {
        let b = [7u8; 16];
        let h = device_hex(&b);
        assert_eq!(device_bytes(&h).unwrap(), b);
    }
}
