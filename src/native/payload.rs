//! Appended payloads: asset + source files stored inside a built executable.
//!
//! A `spid3r build --embed ...` binary carries its program source and its
//! `ui/` assets after the PE image, so the whole application ships as one
//! file. The Windows loader ignores data past the last section, and `link.exe`
//! has already written everything else by the time we append, so the image
//! stays valid.
//!
//! Layout:
//!
//! ```text
//! [ PE image ][ body ][ magic: 16 bytes ][ body_len: u64 LE ]
//! ```
//!
//! `body` is a flat list of entries:
//!
//! ```text
//! for each entry: name_len u32 LE | name utf8 | data_len u64 LE | data
//! ```
//!
//! The body is XOR-obfuscated. This is **not** encryption: the key ships in
//! the binary, so anyone willing to dump process memory recovers the source.
//! It only stops the source from showing up in `strings` output or a text
//! editor, which is what "no readable source" realistically buys.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Marks the start of the trailer. Also identifies a file as carrying a
/// payload, so a plain executable is never mistaken for a packaged one.
const MAGIC: &[u8; 16] = b"SPID3R-PAYLOAD01";

/// Trailer size: magic followed by the little-endian body length.
const TRAILER_LEN: u64 = 24;

/// One embedded file: its path (forward slashes, relative to the source
/// directory) and its raw contents.
pub type Entry = (String, Vec<u8>);

/// The entry that `spid3r build --bundle-runtime` fills with a copy of the
/// toolchain executable. `run_payload()` looks for exactly this name, so the
/// writer (`build`) and the reader (the runtime) must not drift apart.
pub const RUNTIME_ENTRY: &str = if cfg!(windows) {
    "__spid3r_runtime/spid3r.exe"
} else {
    "__spid3r_runtime/spid3r"
};

/// Prefix of the namespace `--bundle-runtime` owns.
const RUNTIME_PREFIX: &str = "__spid3r_runtime/";

/// Rolling XOR key. Obfuscation only — see the module docs.
const KEY: &[u8; 16] = b"sp1d3r-embed-key";

fn obfuscate(bytes: &mut [u8]) {
    for (i, b) in bytes.iter_mut().enumerate() {
        *b ^= KEY[i % KEY.len()] ^ ((i >> 8) as u8);
    }
}

/// Serialise entries into the on-disk body form.
///
/// Duplicate names are rejected: the reader folds entries into a map, so two
/// entries with one name would make which copy wins depend on iteration order
/// (`--embed ui --embed ui/index.html` produced exactly that).
fn encode_body(entries: &[Entry]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (name, data) in entries {
        let name = name.replace('\\', "/");
        if !is_safe_entry_name(&name) {
            return Err(format!(
                "embedded path '{}' is not a safe relative path",
                name
            ));
        }
        if is_reserved_runtime_name(&name) {
            return Err(format!(
                "embedded path '{}' is reserved for the bundled runtime; only '{}' may live there",
                name, RUNTIME_ENTRY
            ));
        }
        if !seen.insert(name.clone()) {
            return Err(format!("duplicate embedded path '{}'", name));
        }
        let name_bytes = name.as_bytes();
        let name_len = u32::try_from(name_bytes.len())
            .map_err(|_| format!("embedded path '{}' is too long", name))?;
        body.extend_from_slice(&name_len.to_le_bytes());
        body.extend_from_slice(name_bytes);
        body.extend_from_slice(&(data.len() as u64).to_le_bytes());
        body.extend_from_slice(data);
    }
    Ok(body)
}

/// Parse a decoded body back into entries.
///
/// Every length is attacker-controlled (a payload is just bytes appended to a
/// file), so the arithmetic is checked: `at + name_len + 8` and `at + data_len`
/// would otherwise wrap on a crafted body and turn a malformed file into a
/// slice-index panic instead of a clean error.
fn decode_body(body: &[u8]) -> Result<Vec<Entry>, String> {
    decode_body_filtered(body, |_| true)
}

/// [`decode_body`], copying only the entries `keep` accepts.
///
/// A payload that carries the runtime (`--bundle-runtime`) is dominated by a
/// ~10 MB executable that the GUI side never needs and the launcher never needs
/// anything else from; parsing without copying keeps one copy out of memory.
fn decode_body_filtered(body: &[u8], keep: impl Fn(&str) -> bool) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    let mut at = 0usize;
    while at < body.len() {
        if body.len() - at < 4 {
            return Err("payload is truncated (entry name length)".to_string());
        }
        let name_len = u32::from_le_bytes(body[at..at + 4].try_into().unwrap()) as usize;
        at += 4;
        // The name and the 8 byte data length must still fit in what is left.
        name_len
            .checked_add(8)
            .and_then(|n| at.checked_add(n))
            .filter(|end| *end <= body.len())
            .ok_or_else(|| "payload is truncated (entry name)".to_string())?;
        let name = String::from_utf8(body[at..at + name_len].to_vec())
            .map_err(|_| "payload contains a non-UTF-8 path".to_string())?;
        if !is_safe_entry_name(&name) {
            return Err(format!("payload contains an unsafe entry path '{}'", name));
        }
        at += name_len;
        let data_len = u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
        at += 8;
        let data_len = usize::try_from(data_len)
            .map_err(|_| format!("payload entry '{}' is too large for this platform", name))?;
        let end = at
            .checked_add(data_len)
            .filter(|end| *end <= body.len())
            .ok_or_else(|| format!("payload is truncated (contents of '{}')", name))?;
        if keep(&name) {
            entries.push((name, body[at..end].to_vec()));
        }
        at = end;
    }
    Ok(entries)
}

/// Reject entry names that could escape the payload's flat namespace.
///
/// Builds only ever write forward-slash paths relative to the source
/// directory, so anything absolute, containing a `..` segment or a backslash
/// (a Windows separator that would alias two distinct keys) is corruption or
/// tampering, not a payload this tool wrote.
fn is_safe_entry_name(name: &str) -> bool {
    if name.is_empty() || name.starts_with('/') || name.contains('\\') {
        return false;
    }
    if name.len() >= 2 && name.as_bytes()[1] == b':' {
        return false; // C:...
    }
    !name.split('/').any(|seg| seg == ".." || seg.is_empty())
}

/// Is `name` inside the namespace `--bundle-runtime` reserves for itself,
/// under a name the launcher will not recognise?
///
/// `run_payload()` extracts and executes exactly [`RUNTIME_ENTRY`], and it
/// does so whether or not `--bundle-runtime` was used. So `--embed
/// __spid3r_runtime/spid3r.exe=some-other-program` would otherwise let a build
/// smuggle an arbitrary executable into the slot the launcher trusts. Only the
/// entry the builder legitimately appends may use that namespace.
fn is_reserved_runtime_name(name: &str) -> bool {
    name.starts_with(RUNTIME_PREFIX) && name != RUNTIME_ENTRY
}

/// Where the payload body starts, if `exe` carries one.
fn body_range(exe: &Path) -> Result<Option<(u64, u64)>, String> {
    let mut file =
        File::open(exe).map_err(|e| format!("cannot open '{}': {}", exe.display(), e))?;
    let len = file
        .metadata()
        .map_err(|e| format!("cannot stat '{}': {}", exe.display(), e))?
        .len();
    if len < TRAILER_LEN {
        return Ok(None);
    }
    file.seek(SeekFrom::End(-(TRAILER_LEN as i64)))
        .map_err(|e| format!("cannot seek in '{}': {}", exe.display(), e))?;
    let mut trailer = [0u8; 24];
    file.read_exact(&mut trailer)
        .map_err(|e| format!("cannot read the trailer of '{}': {}", exe.display(), e))?;
    if &trailer[..16] != MAGIC {
        return Ok(None);
    }
    let body_len = u64::from_le_bytes(trailer[16..24].try_into().unwrap());
    // Checked: a crafted trailer can claim u64::MAX bytes, and
    // `body_len + TRAILER_LEN` would wrap in release builds and pass the
    // guard below, turning a bad file into a wild seek.
    let total = body_len
        .checked_add(TRAILER_LEN)
        .filter(|total| *total <= len)
        .ok_or_else(|| {
            format!(
                "'{}' claims a {} byte payload but is only {} bytes long",
                exe.display(),
                body_len,
                len
            )
        })?;
    Ok(Some((len - total, body_len)))
}

/// Append `entries` to the executable at `exe`.
///
/// An existing payload is replaced rather than stacked, so rebuilding the same
/// output path does not grow the file on every run.
pub fn append(exe: &Path, entries: &[Entry]) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut body = encode_body(entries)?;
    obfuscate(&mut body);

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(exe)
        .map_err(|e| format!("cannot open '{}' for embedding: {}", exe.display(), e))?;

    // Drop a payload left by an earlier build before writing the new one.
    if let Some((start, _)) = body_range(exe)? {
        file.set_len(start)
            .map_err(|e| format!("cannot truncate '{}': {}", exe.display(), e))?;
    }

    file.seek(SeekFrom::End(0))
        .map_err(|e| format!("cannot seek in '{}': {}", exe.display(), e))?;
    file.write_all(&body)
        .map_err(|e| format!("cannot write the payload to '{}': {}", exe.display(), e))?;
    file.write_all(MAGIC)
        .map_err(|e| format!("cannot write the payload to '{}': {}", exe.display(), e))?;
    file.write_all(&(body.len() as u64).to_le_bytes())
        .map_err(|e| format!("cannot write the payload to '{}': {}", exe.display(), e))?;
    file.flush()
        .map_err(|e| format!("cannot flush '{}': {}", exe.display(), e))?;
    Ok(())
}

/// Read every entry embedded in `exe`. `Ok(None)` means the file has no
/// payload at all, which is the normal case for a plain executable.
pub fn read(exe: &Path) -> Result<Option<Vec<Entry>>, String> {
    read_filtered(exe, |_| true)
}

/// Read the payload, keeping only the entries `keep` accepts.
///
/// The body is parsed in full (so a malformed payload is still reported) but
/// only the kept entries are copied.
pub fn read_filtered(
    exe: &Path,
    keep: impl Fn(&str) -> bool,
) -> Result<Option<Vec<Entry>>, String> {
    let Some((start, len)) = body_range(exe)? else {
        return Ok(None);
    };
    let mut file =
        File::open(exe).map_err(|e| format!("cannot open '{}': {}", exe.display(), e))?;
    file.seek(SeekFrom::Start(start))
        .map_err(|e| format!("cannot seek in '{}': {}", exe.display(), e))?;
    let mut body = vec![0u8; len as usize];
    file.read_exact(&mut body)
        .map_err(|e| format!("cannot read the payload of '{}': {}", exe.display(), e))?;
    obfuscate(&mut body);
    decode_body_filtered(&body, keep).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<(String, Vec<u8>)> {
        vec![
            (
                "app.matrix".to_string(),
                b"fn main() { return 0; }".to_vec(),
            ),
            ("ui/index.html".to_string(), b"<html></html>".to_vec()),
            ("ui/app.js".to_string(), b"console.log(1)".to_vec()),
        ]
    }

    #[test]
    fn body_round_trips() {
        let entries = sample();
        let body = encode_body(&entries).unwrap();
        assert_eq!(decode_body(&body).unwrap(), entries);
    }

    #[test]
    fn obfuscation_is_reversible_and_hides_text() {
        let mut bytes = b"study_planner.matrix".to_vec();
        let original = bytes.clone();
        obfuscate(&mut bytes);
        assert_ne!(bytes, original);
        assert!(
            !bytes.windows(7).any(|w| w == b"planner"),
            "obfuscated bytes still contain readable text"
        );
        obfuscate(&mut bytes);
        assert_eq!(bytes, original);
    }

    #[test]
    fn decode_rejects_truncation() {
        let entries = sample();
        let body = encode_body(&entries).unwrap();
        assert!(decode_body(&body[..body.len() - 3]).is_err());
    }

    /// A payload is just bytes appended to a file, so every length field is
    /// attacker-controlled. A huge `data_len` used to wrap `at + data_len` and
    /// panic on the slice index; it must be a clean error.
    #[test]
    fn decode_rejects_impossible_lengths_without_panicking() {
        let mut body = Vec::new();
        body.extend_from_slice(&4u32.to_le_bytes());
        body.extend_from_slice(b"a.js");
        body.extend_from_slice(&u64::MAX.to_le_bytes());
        let err = decode_body(&body).expect_err("u64::MAX length must be rejected");
        assert!(
            err.contains("truncated") || err.contains("too large"),
            "{}",
            err
        );

        // name_len larger than the buffer
        let mut body = Vec::new();
        body.extend_from_slice(&u32::MAX.to_le_bytes());
        body.extend_from_slice(b"x");
        assert!(decode_body(&body).is_err());
    }

    #[test]
    fn decode_rejects_paths_that_escape_the_payload() {
        for bad in [
            "../evil.js",
            "ui/../../evil.js",
            "/etc/passwd",
            "C:/x.js",
            "ui\\x.js",
            "",
        ] {
            let mut body = Vec::new();
            body.extend_from_slice(&(bad.len() as u32).to_le_bytes());
            body.extend_from_slice(bad.as_bytes());
            body.extend_from_slice(&1u64.to_le_bytes());
            body.push(b'x');
            assert!(
                decode_body(&body).is_err(),
                "entry name {:?} must be rejected",
                bad
            );
        }
    }

    #[test]
    fn encode_rejects_duplicate_names() {
        let dup = vec![
            ("ui/index.html".to_string(), b"a".to_vec()),
            ("ui/index.html".to_string(), b"b".to_vec()),
        ];
        let err = encode_body(&dup).expect_err("duplicates must be rejected");
        assert!(err.contains("duplicate"), "{}", err);
    }

    /// `--bundle-runtime` owns `__spid3r_runtime/` because the launcher
    /// extracts and executes whatever sits at [`RUNTIME_ENTRY`]. A build must
    /// not be able to point that slot at an arbitrary file with `--embed`.
    #[test]
    fn encode_rejects_foreign_entries_in_the_runtime_namespace() {
        let smuggled = vec![(
            "__spid3r_runtime/some-other-program".to_string(),
            b"payload".to_vec(),
        )];
        let err = encode_body(&smuggled).expect_err("the runtime namespace is reserved");
        assert!(err.contains("reserved"), "{}", err);

        // The real runtime entry stays valid: that is what `--bundle-runtime`
        // appends.
        let real = vec![(RUNTIME_ENTRY.to_string(), b"runtime".to_vec())];
        assert_eq!(decode_body(&encode_body(&real).unwrap()).unwrap(), real);

        // A file that merely shares the prefix is not in the namespace.
        let neighbour = vec![("__spid3r_runtime_notes.md".to_string(), b"x".to_vec())];
        assert!(encode_body(&neighbour).is_ok());
    }

    /// A trailer that claims more bytes than the file holds must be a clean
    /// error, not an arithmetic wrap followed by a wild seek.
    #[test]
    fn trailer_with_impossible_length_is_rejected() {
        let dir = std::env::temp_dir().join(format!("spid3r_payload_len_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("bad.exe");
        let mut bytes = vec![0u8; 64];
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&exe, &bytes).unwrap();
        assert!(read(&exe).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_file_has_no_payload() {
        let dir = std::env::temp_dir().join(format!("spid3r_payload_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plain = dir.join("plain.bin");
        std::fs::write(&plain, vec![0u8; 512]).unwrap();
        assert!(read(&plain).unwrap().is_none());

        let short = dir.join("short.bin");
        std::fs::write(&short, b"hi").unwrap();
        assert!(read(&short).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_read_and_replace() {
        let dir = std::env::temp_dir().join(format!("spid3r_payload_rw_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("app.exe");
        std::fs::write(&exe, vec![0x4d, 0x5a, 0x90, 0x00]).unwrap();

        append(&exe, &sample()).unwrap();
        assert_eq!(read(&exe).unwrap().unwrap(), sample());

        let second = vec![("only.matrix".to_string(), b"x".to_vec())];
        append(&exe, &second).unwrap();
        assert_eq!(read(&exe).unwrap().unwrap(), second);

        // The original image must survive at the front of the file.
        let bytes = std::fs::read(&exe).unwrap();
        assert_eq!(&bytes[..4], &[0x4d, 0x5a, 0x90, 0x00]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
