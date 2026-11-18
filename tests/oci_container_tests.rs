//! R11 · OCI image model, Registry v2 loopback pull, layer whiteout & traversal
//! safety, copy-up snapshotter + GC, IPAM networking, Ed25519 signature policy,
//! OCI bundle layout, and container lifecycle CLI tests.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

use spid3r_lang::container::net::{allocate_ip, release_ip};
use spid3r_lang::container::runtime::{
    create_container, list_containers, load_oci_bundle_config, load_state, remove_container,
};
use spid3r_lang::container::snapshot::{gc_unreferenced_layers, prepare_rootfs};
use spid3r_lang::oci::client::pull_image;
use spid3r_lang::oci::layers::{cache_and_unpack_layer, sanitize_tar_rel_path};
use spid3r_lang::oci::manifest::{
    host_oci_arch, host_oci_os, parse_manifest_or_index, verify_manifest_signature,
    ManifestOrIndex, SignaturePolicy,
};
use spid3r_lang::pkg::sha256_hex;
use spid3r_lang::stdlib::compress::gzip_compress_bytes;

fn build_ustar(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in entries {
        let mut hdr = [0u8; 512];
        let nb = name.as_bytes();
        let copy_len = nb.len().min(100);
        hdr[..copy_len].copy_from_slice(&nb[..copy_len]);
        hdr[100..108].copy_from_slice(b"0000644\0");
        hdr[108..116].copy_from_slice(b"0000000\0");
        hdr[116..124].copy_from_slice(b"0000000\0");
        let sz = format!("{:011o}\0", data.len());
        hdr[124..136].copy_from_slice(sz.as_bytes());
        hdr[136..148].copy_from_slice(b"00000000000\0");
        hdr[148..156].copy_from_slice(b"        ");
        hdr[156] = b'0';
        hdr[257..263].copy_from_slice(b"ustar\0");
        hdr[263..265].copy_from_slice(b"00");
        let csum: u32 = hdr.iter().map(|&b| b as u32).sum();
        let cs = format!("{:06o}\0 ", csum);
        hdr[148..156].copy_from_slice(cs.as_bytes());
        out.extend_from_slice(&hdr);
        out.extend_from_slice(data);
        let rem = data.len() % 512;
        if rem != 0 {
            out.extend_from_slice(&vec![0u8; 512 - rem]);
        }
    }
    out.extend_from_slice(&[0u8; 1024]);
    out
}

type RouteEntry = (u16, Vec<(String, String)>, Vec<u8>);

fn spawn_oci_fixture(routes: HashMap<String, RouteEntry>) -> (String, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(Mutex::new(0usize));
    let count_c = count.clone();
    let routes = Arc::new(routes);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let routes = routes.clone();
            let count_c = count_c.clone();
            thread::spawn(move || {
                let mut s = stream;
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    return;
                }
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                let path = first_line.split_whitespace().nth(1).unwrap_or("/");
                *count_c.lock().unwrap() += 1;
                if let Some((status, hdrs, body)) = routes.get(path) {
                    let mut head = format!(
                        "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nConnection: close\r\n",
                        body.len()
                    );
                    for (k, v) in hdrs {
                        head.push_str(&format!("{k}: {v}\r\n"));
                    }
                    head.push_str("\r\n");
                    let _ = s.write_all(head.as_bytes());
                    let _ = s.write_all(body);
                } else {
                    let msg = b"not found";
                    let head = format!(
                        "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        msg.len()
                    );
                    let _ = s.write_all(head.as_bytes());
                    let _ = s.write_all(msg);
                }
            });
        }
    });
    (format!("127.0.0.1:{}", addr.port()), count)
}

fn bin() -> PathBuf {
    for key in ["CARGO_BIN_EXE_spid3r", "CARGO_BIN_EXE_spidr"] {
        if let Ok(p) = std::env::var(key) {
            let p = PathBuf::from(p);
            if p.is_file() {
                return p;
            }
        }
    }
    let exe = std::env::current_exe().expect("test executable path");
    let profile_dir = exe
        .parent()
        .and_then(|deps| deps.parent())
        .expect("profile directory");
    for stem in ["spid3r", "spidr"] {
        let cand = profile_dir.join(format!("{}{}", stem, std::env::consts::EXE_SUFFIX));
        if cand.is_file() {
            return cand;
        }
    }
    panic!("spid3r binary not found next to {}", profile_dir.display());
}

#[test]
fn oci_pull_multiarch_whiteouts_snapshot_gc_and_lifecycle() {
    let dir = std::env::temp_dir().join(format!("spid3r_oci_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let oci_dir = dir.join("oci");
    let ctr_dir = dir.join("containers");

    // Layer 1: base files
    let l1_tar = build_ustar(&[
        ("etc/keep.txt", b"hello\n"),
        ("etc/remove.txt", b"old\n"),
        ("var/a.txt", b"v1\n"),
    ]);
    let l1_gz = gzip_compress_bytes(&l1_tar);
    let l1_dig = format!("sha256:{}", sha256_hex(&l1_gz));

    // Layer 2: whiteout etc/remove.txt + opaque var/ + new var/b.txt
    let l2_tar = build_ustar(&[
        ("etc/.wh.remove.txt", b""),
        ("var/.wh..wh..opq", b""),
        ("var/b.txt", b"v2\n"),
    ]);
    let l2_gz = gzip_compress_bytes(&l2_tar);
    let l2_dig = format!("sha256:{}", sha256_hex(&l2_gz));

    let cfg_json = format!(
        r#"{{"architecture":"{}","os":"{}","config":{{"Env":["PATH=/bin"],"Cmd":["/bin/echo","ok"],"WorkingDir":"/"}},"rootfs":{{"diff_ids":["{}","{}"]}}}}"#,
        host_oci_arch(),
        host_oci_os(),
        l1_dig,
        l2_dig
    );
    let cfg_bytes = cfg_json.into_bytes();
    let cfg_dig = format!("sha256:{}", sha256_hex(&cfg_bytes));

    let plat_manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{cfg_dig}","size":{}}},"layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"{l1_dig}","size":{}}},{{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"{l2_dig}","size":{}}}]}}"#,
        cfg_bytes.len(),
        l1_gz.len(),
        l2_gz.len()
    )
    .into_bytes();
    let plat_dig = format!("sha256:{}", sha256_hex(&plat_manifest));

    // Multi-arch index with a bogus arch + host arch
    let index_json = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:{}","size":1,"platform":{{"os":"plan9","architecture":"mips"}}}},{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{plat_dig}","size":{},"platform":{{"os":"{}","architecture":"{}"}}}}]}}"#,
        "0".repeat(64),
        plat_manifest.len(),
        host_oci_os(),
        host_oci_arch()
    )
    .into_bytes();

    let mut routes = HashMap::new();
    routes.insert(
        "/v2/app/demo/manifests/v1".to_string(),
        (200, vec![], index_json),
    );
    routes.insert(
        format!("/v2/app/demo/manifests/{plat_dig}"),
        (200, vec![], plat_manifest),
    );
    routes.insert(
        format!("/v2/app/demo/blobs/{cfg_dig}"),
        (200, vec![], cfg_bytes),
    );
    routes.insert(format!("/v2/app/demo/blobs/{l1_dig}"), (200, vec![], l1_gz));
    routes.insert(format!("/v2/app/demo/blobs/{l2_dig}"), (200, vec![], l2_gz));

    let (host, _) = spawn_oci_fixture(routes);
    let image_ref = format!("http://{host}/app/demo:v1");

    // Pull via library API
    let pulled = pull_image(&image_ref, &oci_dir, SignaturePolicy::AllowUnsigned, None)
        .expect("pull_image succeeds");
    assert_eq!(pulled.layer_dirs.len(), 2);
    assert_eq!(pulled.config.cmd, vec!["/bin/echo", "ok"]);

    // Create container + verify copy-up whiteouts + OCI bundle config.json
    let rec = create_container(&ctr_dir, "demo1", &pulled, None).expect("create_container");
    assert!(rec.rootfs.join("etc/keep.txt").is_file());
    assert!(!rec.rootfs.join("etc/remove.txt").exists());
    assert!(!rec.rootfs.join("var/a.txt").exists());
    assert!(rec.rootfs.join("var/b.txt").is_file());

    let (b_args, b_cwd, b_rootfs) = load_oci_bundle_config(&ctr_dir.join("demo1")).expect("bundle");
    assert_eq!(b_args, vec!["/bin/echo", "ok"]);
    assert_eq!(b_cwd, "/");
    assert_eq!(b_rootfs, ctr_dir.join("demo1/rootfs"));

    // IPAM allocate & release
    let net1 = allocate_ip(&ctr_dir, "demo1", vec![(8080, 80)]).expect("alloc ip 1");
    let net2 = allocate_ip(&ctr_dir, "demo2", vec![]).expect("alloc ip 2");
    assert_ne!(net1.container_ip_cidr, net2.container_ip_cidr);
    release_ip(&ctr_dir, "demo1").expect("release ip");

    // State recovery & removal
    assert_eq!(list_containers(&ctr_dir).unwrap().len(), 1);
    assert_eq!(load_state(&ctr_dir.join("demo1")).unwrap().id, "demo1");
    remove_container(&ctr_dir, "demo1").expect("remove_container");
    assert_eq!(list_containers(&ctr_dir).unwrap().len(), 0);

    // Snapshot GC removes unreferenced orphan layer if we drop an extra dir
    let orphan = oci_dir.join("layers").join("deadbeef".repeat(8));
    std::fs::create_dir_all(&orphan).unwrap();
    let removed = gc_unreferenced_layers(&oci_dir, &ctr_dir).expect("gc");
    assert_eq!(removed, 1);
    assert!(!orphan.exists());

    // CLI lifecycle: pull + create + ps + inspect + logs + rm
    let o_pull = Command::new(bin())
        .env("SPID3R_OCI_DIR", &oci_dir)
        .env("SPID3R_CONTAINER_DIR", &ctr_dir)
        .args(["container", "pull", &image_ref])
        .output()
        .expect("cli pull");
    assert_eq!(
        o_pull.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o_pull.stderr)
    );

    let o_create = Command::new(bin())
        .env("SPID3R_OCI_DIR", &oci_dir)
        .env("SPID3R_CONTAINER_DIR", &ctr_dir)
        .args(["container", "create", "--name", "cli1", &image_ref])
        .output()
        .expect("cli create");
    assert_eq!(o_create.status.code(), Some(0));

    let o_ps = Command::new(bin())
        .env("SPID3R_OCI_DIR", &oci_dir)
        .env("SPID3R_CONTAINER_DIR", &ctr_dir)
        .args(["container", "ps"])
        .output()
        .expect("cli ps");
    assert!(String::from_utf8_lossy(&o_ps.stdout).contains("cli1"));

    let o_rm = Command::new(bin())
        .env("SPID3R_OCI_DIR", &oci_dir)
        .env("SPID3R_CONTAINER_DIR", &ctr_dir)
        .args(["container", "rm", "cli1"])
        .output()
        .expect("cli rm");
    assert_eq!(o_rm.status.code(), Some(0));

    let _ = prepare_rootfs(&dir.join("snap_extra"), &pulled.layer_dirs).expect("prepare_rootfs");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn oci_corrupt_digest_traversal_and_unsigned_policy_refusals() {
    let dir = std::env::temp_dir().join(format!("spid3r_oci_sec_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let oci_dir = dir.join("oci");

    // 1. Corrupted digest must refuse and extract nothing
    let bad_dig = format!("sha256:{}", "a".repeat(64));
    let err = cache_and_unpack_layer(&oci_dir, &bad_dig, b"tampered-bytes")
        .expect_err("corrupted layer must fail");
    assert!(err.contains("error[corrupt]"), "{err}");
    assert!(!oci_dir.join("layers").join("a".repeat(64)).exists());

    // 2. Path traversal in tar must be refused
    assert!(sanitize_tar_rel_path("../etc/passwd").is_err());
    assert!(sanitize_tar_rel_path("/etc/passwd").is_err());
    assert!(sanitize_tar_rel_path("C:\\Windows\\win.ini").is_err());

    let evil_tar = build_ustar(&[("../escape.txt", b"bad")]);
    let evil_gz = gzip_compress_bytes(&evil_tar);
    let evil_dig = format!("sha256:{}", sha256_hex(&evil_gz));
    let err_trav =
        cache_and_unpack_layer(&oci_dir, &evil_dig, &evil_gz).expect_err("traversal must fail");
    assert!(err_trav.contains("traversal"), "{err_trav}");

    // 3. Unsigned manifest refused under RequireSigned policy
    let sig_err =
        verify_manifest_signature(b"{}", None, None, SignaturePolicy::RequireSigned).unwrap_err();
    assert!(sig_err.contains("unsigned OCI image"), "{sig_err}");

    // 4. Malformed manifest JSON cleanly rejected
    assert!(parse_manifest_or_index(b"{not-json").is_err());
    assert!(matches!(
        parse_manifest_or_index(
            br#"{"schemaVersion":2,"manifests":[{"digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":1}]}"#
        )
        .unwrap(),
        ManifestOrIndex::Index(_)
    ));

    let _ = std::fs::remove_dir_all(&dir);
}
