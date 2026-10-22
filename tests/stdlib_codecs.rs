//! The 2026-10-22 stdlib additions: hex/base32 natives (alongside base64),
//! `stdlib/toml.matrix`, `stdlib/semver.matrix`, and `stdlib/uuid.matrix`.
//! Every check runs on BOTH engines (the pure-Spid3r modules must behave
//! identically everywhere).

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

fn run(engine: &str, name: &str, src: &str) -> (i32, String, String) {
    let dir = std::env::temp_dir().join("spid3r_codec");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{}.matrix", name));
    std::fs::write(&path, src).expect("write case");
    let o = Command::new(bin())
        .args(["run", "--engine", engine, path.to_str().unwrap()])
        .output()
        .expect("spawn spid3r");
    let _ = std::fs::remove_file(&path);
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).to_string(),
        String::from_utf8_lossy(&o.stderr).to_string(),
    )
}

const HEX_B32: &str = "\
fn main() {
    // RFC 4648 section 10 base32 vectors.
    println(base32_encode(\"\"));
    println(base32_encode(\"f\"));
    println(base32_encode(\"fo\"));
    println(base32_encode(\"foo\"));
    println(base32_encode(\"foob\"));
    println(base32_encode(\"fooba\"));
    println(base32_encode(\"foobar\"));
    // Hex: lowercase two digits per byte; round-trips through bytes.
    println(hex_encode(\"hi\"));
    println(hex_encode(\"\"));
    println(hex_decode(\"6869\").to_base64());
    println(hex_encode(hex_decode(\"deadBEEF\")));
    return 0;
}
";

#[test]
fn hex_and_base32_match_rfc4648_on_both_engines() {
    let expected = "\n\
MY======
MZXQ====
MZXW6===
MZXW6YQ=
MZXW6YTB
MZXW6YTBOI======
6869

aGk=
deadbeef
";
    for engine in ["vm", "tree"] {
        let (code, stdout, stderr) = run(engine, "hexb32", HEX_B32);
        assert_eq!(code, 0, "[{}] stderr: {}", engine, stderr);
        assert_eq!(stdout, expected, "[{}]", engine);
    }
}

#[test]
fn hex_decode_rejects_invalid_input() {
    let src = "\
fn main() {
    println(hex_decode(\"abc\"));
    return 0;
}
";
    for engine in ["vm", "tree"] {
        let (code, _, stderr) = run(engine, "hexbad", src);
        assert_eq!(code, 1, "[{}]", engine);
        assert!(
            stderr.contains("hex_decode"),
            "[{}] stderr: {}",
            engine,
            stderr
        );
    }
}

const SEMVER: &str = "\
import \"semver\" as sv;

fn main() {
    println(sv.compare(\"1.2.3\", \"1.2.3\"));
    println(sv.compare(\"1.2.3\", \"1.10.0\"));
    println(sv.compare(\"2.0.0\", \"1.9.9\"));
    println(sv.compare(\"1.0.0-alpha\", \"1.0.0\"));
    println(sv.compare(\"1.0.0-alpha.1\", \"1.0.0-alpha.2\"));
    println(sv.compare(\"1.0.0-alpha.2\", \"1.0.0-alpha.10\"));
    println(sv.compare(\"1.0.0-1\", \"1.0.0-alpha\"));
    println(sv.compare(\"1.0.0+build.9\", \"1.0.0\"));
    println(sv.satisfies(\"1.2.3\", \">=1.0.0 <2.0.0\"));
    println(sv.satisfies(\"2.1.0\", \">=1.0.0 <2.0.0\"));
    println(sv.satisfies(\"1.5.0\", \"1.5.0\"));
    let p = sv.parse(\"1.2.3-alpha.1+build\");
    println(p[\"major\"]);
    println(p[\"prerelease\"][1]);
    println(sv.core(\"1.2.3-rc.1\"));
    return 0;
}
";

#[test]
fn semver_orders_per_spec_on_both_engines() {
    let expected = "0\n-1\n1\n-1\n-1\n-1\n-1\n0\ntrue\nfalse\ntrue\n1\n1\n1.2.3\n";
    for engine in ["vm", "tree"] {
        let (code, stdout, stderr) = run(engine, "semver_case", SEMVER);
        assert_eq!(code, 0, "[{}] stderr: {}", engine, stderr);
        assert_eq!(stdout, expected, "[{}]", engine);
    }
}

const UUID: &str = "\
import \"uuid\" as u;

fn main() {
    let a = u.uuid_v4();
    let b = u.uuid_v4();
    // Shape: 8-4-4-4-12, version nibble 4, variant in 89ab.
    let ok_shape = len(a) == 36 and a.slice(14, 15) == \"4\";
    let variant = a.slice(19, 20);
    let ok_variant = variant == \"8\" or variant == \"9\" or variant == \"a\" or variant == \"b\";
    let ok_unique = a != b;
    if ok_shape and ok_variant and ok_unique {
        println(\"ok\");
    } else {
        println(\"bad: \" + a);
    }
    return 0;
}
";

#[test]
fn uuid_v4_has_the_rfc_shape_on_both_engines() {
    for engine in ["vm", "tree"] {
        let (code, stdout, stderr) = run(engine, "uuid_case", UUID);
        assert_eq!(code, 0, "[{}] stderr: {}", engine, stderr);
        assert_eq!(stdout.trim(), "ok", "[{}] stdout: {}", engine, stdout);
    }
}
