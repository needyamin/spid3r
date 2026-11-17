//! Shared offline TLS test fixture for `tests/tls_server.rs` and `tests/tls_fetch.rs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

#[allow(dead_code)]
pub const LOCALHOST_P12: &[u8] = include_bytes!("../data/localhost.p12");
#[allow(dead_code)]
pub const LOCALHOST_CERT_PEM: &[u8] = include_bytes!("../data/localhost_cert.pem");
#[allow(dead_code)]
pub const LOCALHOST_CERT_DER: &[u8] = include_bytes!("../data/localhost_cert.der");

static FIXTURE_SEQ: AtomicU32 = AtomicU32::new(0);

#[allow(dead_code)]
pub struct TempTlsFixture {
    pub dir: PathBuf,
    pub p12_path: PathBuf,
    pub cert_pem_path: PathBuf,
    pub dummy_key_path: PathBuf,
}

impl TempTlsFixture {
    #[allow(dead_code)]
    pub fn new() -> Self {
        let seq = FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("spid3r_tls_fixture_{}_{}", std::process::id(), seq));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp tls fixture dir");

        let p12_path = dir.join("localhost.p12");
        let cert_pem_path = dir.join("localhost_cert.pem");
        let dummy_key_path = dir.join("dummy_key.pem");

        std::fs::write(&p12_path, LOCALHOST_P12).expect("write localhost.p12");
        std::fs::write(&cert_pem_path, LOCALHOST_CERT_PEM).expect("write localhost_cert.pem");
        std::fs::write(
            &dummy_key_path,
            b"-----BEGIN PRIVATE KEY-----\nMIIBVQIBADANBgkqhkiG9w0BAQEFAASCAT8=\n-----END PRIVATE KEY-----\n",
        )
        .expect("write dummy_key.pem");

        Self {
            dir,
            p12_path,
            cert_pem_path,
            dummy_key_path,
        }
    }
}

impl Drop for TempTlsFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[allow(dead_code)]
pub fn escape_matrix_str(path: &Path) -> String {
    path.to_str().unwrap().replace('\\', "/")
}
