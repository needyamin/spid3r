//! Second wave of §3.5 coverage: zip archives, MessagePack/CBOR/protobuf,
//! YAML, bcrypt/argon2, PNG/WAV/PDF, the math extras, and the operator
//! overloading additions (`__neg__`, reflected `__r*`).
//!
//! Every value asserted here was cross-checked against an outside
//! implementation when the feature landed (Python's `zipfile`, OpenSSL's
//! `ARGON2*`, the OpenBSD bcrypt vectors, a from-scratch Python PNG decoder).

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::Arc;

fn run_tree(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("ext2.matrix"))?;
    semantic::check_file(&program, true, "ext2.matrix")?;
    let interp = Interp::new(Arc::from("ext2.matrix"), modules::default_module_dirs());
    interp.run_program(&program)
}

fn run_vm(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("ext2.matrix"))?;
    semantic::check_file(&program, true, "ext2.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("ext2.matrix"), dirs);
    vm.run_program(&program)
}

fn both(src: &str) {
    assert_eq!(run_tree(src).unwrap(), 0, "tree engine");
    assert_eq!(run_vm(src).unwrap(), 0, "vm engine");
}

const ZIP: &str = r#"
fn main() {
    let files = list(
        map("name", "a.txt", "data", "hello zip, hello zip, hello zip"),
        map("name", "dir/b.bin", "data", bytes("").from_hex("00010203feff")),
        map("name", "empty.txt", "data", "")
    );
    let z = zip_create(files);
    assert_eq(z.slice(0, 2).to_hex(), "504b"); // "PK"
    let names = zip_list(z).unwrap();
    assert_eq(len(names), 3);
    assert_eq(names[0], "a.txt");
    let m = zip_read(z).unwrap();
    assert_eq(m["a.txt"].to_string().unwrap(), "hello zip, hello zip, hello zip");
    assert_eq(m["dir/b.bin"].to_hex(), "00010203feff");
    assert_eq(m["empty.txt"].len(), 0);
    assert_eq(is_failure(zip_read("nope")), true);
    return 0;
}
"#;

const BINARY_FORMATS: &str = r#"
fn main() {
    // MessagePack fixed encodings from the spec.
    assert_eq(msgpack_encode(0).to_hex(), "00");
    assert_eq(msgpack_encode(128).to_hex(), "cc80");
    assert_eq(msgpack_encode("abc").to_hex(), "a3616263");
    let v = map("a", 1, "b", list(1, 2, 3), "c", true);
    let mp = msgpack_encode(v);
    let back = msgpack_decode(mp).unwrap();
    assert_eq(back["a"], 1);
    assert_eq(back["b"][2], 3);
    assert_eq(back["c"], true);

    // CBOR fixed encodings from RFC 8949.
    assert_eq(cbor_encode(0).to_hex(), "00");
    assert_eq(cbor_encode(23).to_hex(), "17");
    assert_eq(cbor_encode(24).to_hex(), "1818");
    assert_eq(cbor_encode(-1).to_hex(), "20");
    assert_eq(cbor_encode(true).to_hex(), "f5");
    let cb = cbor_encode(v);
    let cb2 = cbor_decode(cb).unwrap();
    assert_eq(cb2["b"][0], 1);

    // Protobuf wire format: the canonical field-1 varint-150 example.
    assert_eq(pb_encode(map(1, 150)).to_hex(), "089601");
    let pb = pb_decode(pb_encode(map(1, 150, 2, "hi"))).unwrap();
    assert_eq(pb[1][0], 150);
    assert_eq(pb[2][0].to_string().unwrap(), "hi");
    return 0;
}
"#;

const YAML: &str = r#"
import "yaml";

fn main() {
    // Built with join() rather than \n escapes so the test does not depend on
    // the lexer's escape handling.
    let lines = list(
        "name: app",
        "port: 8080",
        "live: true",
        "none: ~",
        "tags:",
        "  - a",
        "  - b",
        "nested:",
        "  host: a.io",
        "  ports:",
        "    - 80",
        "    - 443",
        "flow: [1, 2, 3]",
        "fmap: {x: 1}",
        "servers:",
        "  - name: one",
        "    port: 1",
        "  - name: two",
        "    port: 2"
    );
    let text = lines.join("\n");
    assert_eq(len(text), 196);
    assert_eq(len(lines), 19);
    let y = yaml_parse(text);
    assert_eq(y["name"], "app");
    assert_eq(y["port"], 8080);
    assert_eq(y["live"], true);
    assert_eq(y["none"], null);
    assert_eq(y["tags"][1], "b");
    assert_eq(y["nested"]["host"], "a.io");
    assert_eq(y["nested"]["ports"][1], 443);
    assert_eq(y["flow"][2], 3);
    assert_eq(y["fmap"]["x"], 1);
    assert_eq(y["servers"][0]["name"], "one");
    assert_eq(y["servers"][1]["port"], 2);

    // write -> parse preserves the structure
    let round = yaml_parse(yaml_write(y));
    assert_eq(round["name"], "app");
    assert_eq(round["nested"]["ports"][0], 80);
    assert_eq(round["servers"][1]["name"], "two");

    // malformed input throws
    let caught = 0;
    try {
        yaml_parse("a: 1\n- b");
    } catch (e) {
        caught = 1;
    }
    assert_eq(caught, 1);
    return 0;
}
"#;

const PASSWORD_HASHING: &str = r#"
fn main() {
    // bcrypt: the OpenBSD vector (cost 5 keeps the test fast).
    assert_eq(
        bcrypt_hash("U*U", 5, "CCCCCCCCCCCCCCCCCCCCC."),
        "$2b$05$CCCCCCCCCCCCCCCCCCCCC.E5YPO9kmyuRGyh0XouQYb4YMJKvyOeW"
    );
    assert_eq(
        bcrypt_verify("U*U", "$2a$05$CCCCCCCCCCCCCCCCCCCCC.E5YPO9kmyuRGyh0XouQYb4YMJKvyOeW"),
        true
    );
    assert_eq(
        bcrypt_verify("wrong", "$2a$05$CCCCCCCCCCCCCCCCCCCCC.E5YPO9kmyuRGyh0XouQYb4YMJKvyOeW"),
        false
    );
    let fresh = bcrypt_hash("hunter2", 5);
    assert_eq(bcrypt_verify("hunter2", fresh), true);
    assert_eq(bcrypt_verify("hunter3", fresh), false);

    // argon2id against the OpenSSL reference value.
    assert_eq(
        argon2("password", "somesalt12345678", 3, 32, 4, "id", 32),
        "cd342db86d5658c8e3267ba1c32c14f66bd95c49ca2c842dbff0e298d37817c9"
    );
    assert_eq(
        argon2("password", "somesalt12345678", 3, 32, 4, "i", 32),
        "ae04861538d7375fa00ec326ee8c7f679cd7020e24d634a041946dafa3bc9568"
    );
    assert_eq(
        argon2("password", "somesalt12345678", 3, 32, 4, "d", 32),
        "fd4bedc4e13fbd88c0ddbfa94be889f28a1b438e0bae4c390864cd656d52ade0"
    );
    return 0;
}
"#;

const MEDIA: &str = r#"
fn main() {
    // PNG: encode then decode returns the same pixels.
    let px = bytes("").from_hex("ff000000ff000000ff010203040506070809");
    let png = png_encode(3, 2, 3, px);
    assert_eq(png.slice(0, 4).to_hex(), "89504e47");
    let img = png_decode(png).unwrap();
    assert_eq(img["width"], 3);
    assert_eq(img["height"], 2);
    assert_eq(img["channels"], 3);
    assert_eq(img["data"].to_hex(), px.to_hex());
    assert_eq(is_failure(png_decode("nope")), true);

    // WAV round-trip.
    let samples = bytes("").from_hex("0000010002000300");
    let wav = wav_encode(44100, 1, 16, samples);
    let w = wav_decode(wav).unwrap();
    assert_eq(w["sample_rate"], 44100);
    assert_eq(w["channels"], 1);
    assert_eq(w["bits"], 16);
    assert_eq(w["data"].to_hex(), samples.to_hex());

    // PDF structure.
    let pdf = pdf_write(list("BT /F1 12 Tf 72 720 Td (Hello) Tj ET"), 595, 842);
    // The header carries a binary marker after %PDF-1.4, so the file is not
    // valid UTF-8; check the ASCII prefix and the trailer via slices.
    assert_eq(pdf.slice(0, 8).to_string().unwrap(), "%PDF-1.4");
    assert_eq(pdf.slice(pdf.len() - 6, pdf.len()).to_string().unwrap(), "%%EOF\n");
    assert_eq(pdf.len() > 300, true);
    assert_eq(pdf_escape("a(b)c"), "a\\(b\\)c");
    return 0;
}
"#;

const MATHX: &str = r#"
import "mathx" as mx;

fn main() {
    // One-way ANOVA: F = 38.4 on three separated groups.
    let a = mx.anova_oneway(list(list(1.0, 2.0, 3.0, 4.0), list(5.0, 6.0, 7.0, 8.0), list(9.0, 10.0, 11.0, 12.0)));
    assert_eq(abs(a["f"] - 38.4) < 1e-9, true);
    assert_eq(a["df_between"], 2);
    assert_eq(a["df_within"], 9);
    assert_eq(a["p"] < 0.001, true);

    // Mann-Whitney: fully separated samples give U = 0.
    let mw = mx.mann_whitney(list(1.0, 2.0, 3.0), list(4.0, 5.0, 6.0));
    assert_eq(mw["u"], 0.0);

    // KS: disjoint supports give D = 1.
    let ks = mx.ks_test(list(1.0, 2.0, 3.0), list(10.0, 20.0, 30.0));
    assert_eq(abs(ks["d"] - 1.0) < 1e-12, true);

    // Interpolation and convolution.
    assert_eq(mx.interp_linear(list(0.0, 10.0), list(0.0, 100.0), 2.5), 25.0);
    assert_eq(mx.convolve(list(1.0, 2.0), list(3.0, 4.0)), list(3.0, 10.0, 8.0));

    // Sparse round-trip and mat-vec.
    let d = matrix(3, 3);
    d.set(0, 0, 1.0);
    d.set(1, 1, 2.0);
    d.set(2, 2, 3.0);
    let s = mx.sparse_from_dense(d);
    assert_eq(mx.sparse_nnz(s), 3);
    let v = vector(list(1.0, 1.0, 1.0));
    assert_eq(mx.sparse_mul_vector(s, v).to_list(), list(1.0, 2.0, 3.0));
    assert_eq(mx.sparse_to_dense(s).get(2, 2), 3.0);

    // Random draws stay in range.
    assert_eq(mx.random_exponential(1.0) > 0.0, true);
    assert_eq(mx.random_poisson(3.0) >= 0, true);
    assert_eq(len(mx.shuffle(list(1, 2, 3, 4))), 4);
    return 0;
}
"#;

const OPERATORS: &str = r#"
import "bigint" as bi;

fn main() {
    let n = 100n;
    // Unary minus via __neg__.
    assert_eq(bi.to_string(-n), "-100");
    assert_eq(bi.to_string(-n + 1), "-99");
    assert_eq(bi.to_string(-(-n)), "100");
    // Reflected operators: the bigint is on the right.
    assert_eq(bi.to_string(1 + n), "101");
    assert_eq(bi.to_string(1 - n), "-99");
    assert_eq(bi.to_string(2 * n), "200");
    assert_eq(bi.to_string(200 / n), "2");
    return 0;
}
"#;

#[test]
fn zip_on_both_engines() {
    both(ZIP);
}

#[test]
fn binary_formats_on_both_engines() {
    both(BINARY_FORMATS);
}

#[test]
fn yaml_on_both_engines() {
    both(YAML);
}

#[test]
fn password_hashing_on_both_engines() {
    both(PASSWORD_HASHING);
}

#[test]
fn media_on_both_engines() {
    both(MEDIA);
}

#[test]
fn mathx_on_both_engines() {
    both(MATHX);
}

#[test]
fn operator_overloads_on_both_engines() {
    both(OPERATORS);
}
