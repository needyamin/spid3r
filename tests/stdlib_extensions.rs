//! Section 3.5 extensions: compression (DEFLATE/gzip/zlib), scrypt,
//! `secure_compare`, `chr`/`ord`, the TOML writer, FFT, the distribution
//! functions and the XML parser. Everything runs on BOTH engines.
//!
//! Values here are not self-referential: the CRC/Adler constants, the scrypt
//! digest and the distribution numbers were cross-checked against CPython
//! (`zlib`, `hashlib`, `statistics`, `math`) when the features landed.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::Arc;

fn run_tree(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("extensions.matrix"))?;
    semantic::check_file(&program, true, "extensions.matrix")?;
    let interp = Interp::new(
        Arc::from("extensions.matrix"),
        modules::default_module_dirs(),
    );
    interp.run_program(&program)
}

fn run_vm(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("extensions.matrix"))?;
    semantic::check_file(&program, true, "extensions.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("extensions.matrix"), dirs);
    vm.run_program(&program)
}

fn both(src: &str) {
    assert_eq!(run_tree(src).unwrap(), 0, "tree engine");
    assert_eq!(run_vm(src).unwrap(), 0, "vm engine");
}

/// Shared prologue: float comparison without exact-equality traps.
const COMPRESSION: &str = r#"
fn main() {
    // Checksums: the canonical CRC-32 and Adler-32 test strings.
    assert_eq(crc32("123456789"), 3421780262);
    assert_eq(adler32("Wikipedia"), 300286872);
    assert_eq(crc32(""), 0);

    let text = "the quick brown fox jumps over the lazy dog. ";
    let big = text + text + text + text + text;

    // Raw DEFLATE round-trip.
    let raw = deflate(big);
    assert_eq(inflate(raw).unwrap().to_string().unwrap(), big);
    assert_eq(len(raw) < len(big), true);

    // gzip round-trip; the container must carry the magic bytes.
    let gz = gzip_compress(big);
    assert_eq(gz.slice(0, 2).to_hex(), "1f8b");
    assert_eq(gunzip(gz).unwrap().to_string().unwrap(), big);

    // zlib round-trip; 0x78 0x9c is the default header.
    let zl = zlib_compress(big);
    assert_eq(zl.slice(0, 2).to_hex(), "789c");
    assert_eq(unzlib(zl).unwrap().to_string().unwrap(), big);

    // Binary payloads survive untouched (bytes in, bytes out).
    let bin = bytes("").from_hex("00010203feff");
    assert_eq(inflate(deflate(bin)).unwrap().to_hex(), "00010203feff");

    // Corrupt and non-compressed input must fail, not panic.
    assert_eq(is_failure(gunzip("not a gzip stream")), true);
    assert_eq(is_failure(unzlib("nope")), true);
    assert_eq(is_failure(inflate("nope")), true);
    return 0;
}
"#;

const CRYPTO: &str = r#"
fn main() {
    // Constant-time comparison.
    assert_eq(secure_compare("abc", "abc"), true);
    assert_eq(secure_compare("abc", "abd"), false);
    assert_eq(secure_compare("abc", "abcd"), false);
    assert_eq(secure_compare("", ""), true);
    let b1 = bytes("");
    assert_eq(secure_compare(b1.from_hex("00ff"), b1.from_hex("00ff")), true);

    // scrypt, RFC 7914 §12 vector 1.
    assert_eq(
        scrypt("", "", 16, 1, 1, 64),
        "77d6576238657b203b19ca42c18a0497f16b4844e3074ae8dfdffa3fede21442fcd0069ded0948f8326a753a0fc81f17e8d3e0fb2e0d3628cf35e20c38d18906"
    );
    // Default dklen is 32 bytes (64 hex chars).
    assert_eq(len(scrypt("pw", "salt", 16, 1, 1)), 64);
    // Bad parameters are errors, not panics.
    let failed = false;
    try {
        scrypt("p", "s", 15, 1, 1, 32);
    } catch (e) {
        failed = true;
    }
    assert_eq(failed, true);

    // chr/ord round-trip, including non-ASCII.
    assert_eq(chr(65), "A");
    assert_eq(ord("A"), 65);
    assert_eq(chr(233), "é");
    assert_eq(ord("é"), 233);
    return 0;
}
"#;

const TOML_WRITER: &str = r#"
import "toml";

fn near(a, b, tol) {
    if abs(a - b) > tol {
        throw "not near: " + string(a) + " vs " + string(b);
    }
}

fn main() {
    let src = "title = \"app\"\nport = 8080\nlive = true\ntags = [\"a\", \"b\"]\n\n[server]\nhost = \"a.io\"\n\n[[worker]]\nname = \"w1\"\n\n[[worker]]\nname = \"w2\"\n";
    let doc = toml_parse(src);
    let text = toml_write(doc);
    let back = toml_parse(text);
    assert_eq(back["title"], "app");
    assert_eq(back["port"], 8080);
    assert_eq(back["live"], true);
    assert_eq(back["tags"][1], "b");
    assert_eq(toml_get(back, "server.host"), "a.io");
    assert_eq(len(back["worker"]), 2);
    assert_eq(back["worker"][1]["name"], "w2");

    // Quotes and newlines are escaped so the document re-parses.
    let tricky = map("k", "say \"hi\"\nnext\tline");
    assert_eq(toml_parse(toml_write(tricky))["k"], "say \"hi\"\nnext\tline");
    return 0;
}
"#;

const FFT_AND_DIST: &str = r#"
import "stats" as st;

fn near(a, b, tol) {
    if abs(a - b) > tol {
        throw "not near: " + string(a) + " vs " + string(b);
    }
}

fn main() {
    // A DC-only signal puts all energy in bin 0.
    let f = fft(vector(list(1.0, 0.0, 0.0, 0.0)));
    assert_eq(f["n"], 4);
    near(f["re"].to_list()[0], 1.0, 1e-12);
    near(f["re"].to_list()[1], 1.0, 1e-12);

    // Impulse -> flat spectrum; inverse restores the signal.
    let sig = vector(list(1.0, 2.0, 3.0, 4.0, 3.0, 2.0, 1.0, 0.0));
    let g = fft(sig);
    near(g["re"].to_list()[0], 16.0, 1e-9);
    near(g["re"].to_list()[1], -4.82842712474619, 1e-9);
    near(g["im"].to_list()[1], -4.82842712474619, 1e-9);
    let back = ifft(g["re"], g["im"]);
    near(back["re"].to_list()[3], 4.0, 1e-9);

    // Inverse normal CDF (Acklam, ~1e-9).
    near(st.normal_quantile(0.975), 1.9599639845400536, 1e-8);
    near(st.normal_quantile(0.025), -1.9599639845400538, 1e-8);
    near(st.normal_quantile(0.975, 100.0, 15.0), 129.3994597681008, 1e-6);

    // Distributions against closed forms.
    near(st.uniform_pdf(0.5, 0.0, 1.0), 1.0, 1e-12);
    near(st.uniform_cdf(0.25, 0.0, 1.0), 0.25, 1e-12);
    near(st.exponential_pdf(1.0, 2.0), 0.2706705664732254, 1e-12);
    near(st.exponential_cdf(1.0, 2.0), 0.8646647167633873, 1e-12);
    near(st.binomial_pmf(3, 10, 0.5), 0.1171875, 1e-12);
    near(st.poisson_pmf(2, 3.0), 0.22404180765538775, 1e-12);
    near(st.lgamma(5.0), 3.178053830347945, 1e-12);

    // Edges: out-of-support values are zero, not errors.
    near(st.binomial_pmf(-1, 10, 0.5), 0.0, 1e-12);
    near(st.binomial_pmf(11, 10, 0.5), 0.0, 1e-12);
    near(st.poisson_pmf(-1, 3.0), 0.0, 1e-12);
    return 0;
}
"#;

const XML: &str = r#"
import "xml";

fn main() {
    let src = "<?xml version=\"1.0\"?><!-- c --><catalog count=\"2\"><book id=\"b1\" lang='en'>Dune &amp; Messiah</book><book id=\"b2\"><title>Neuromancer</title></book><empty/><![CDATA[raw <x>]]></catalog>";
    let doc = xml_parse(src);
    assert_eq(doc["name"], "catalog");
    assert_eq(xml_attr(doc, "count"), "2");
    assert_eq(xml_attr(doc, "absent"), null);
    let books = xml_find(doc, "book");
    assert_eq(len(books), 2);
    assert_eq(xml_attr(books[0], "id"), "b1");
    assert_eq(xml_attr(books[0], "lang"), "en");
    assert_eq(xml_text(books[0]), "Dune & Messiah");
    assert_eq(xml_text(xml_find(doc, "title")[0]), "Neuromancer");
    assert_eq(doc["text"], "raw <x>");

    // Numeric character references, decimal and hex.
    let d = xml_parse("<a t=\"&#65;&#x42;\">caf&#233;</a>");
    assert_eq(xml_attr(d, "t"), "AB");
    assert_eq(xml_text(d), "café");

    // Serialization round-trips through the parser.
    assert_eq(xml_parse(xml_write(books[1]))["name"], "book");

    // Malformed documents throw (and are catchable) rather than mis-parsing.
    let caught = 0;
    for bad in list("<a><b></a>", "<a>", "not xml", "<a href=x/>", "<!DOCTYPE a><a/>") {
        try {
            xml_parse(bad);
        } catch (e) {
            caught = caught + 1;
        }
    }
    assert_eq(caught, 5);
    return 0;
}
"#;

#[test]
fn compression_on_both_engines() {
    both(COMPRESSION);
}

#[test]
fn crypto_on_both_engines() {
    both(CRYPTO);
}

#[test]
fn toml_writer_on_both_engines() {
    both(TOML_WRITER);
}

#[test]
fn fft_and_distributions_on_both_engines() {
    both(FFT_AND_DIST);
}

#[test]
fn xml_on_both_engines() {
    both(XML);
}
