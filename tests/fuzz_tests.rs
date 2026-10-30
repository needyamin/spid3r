//! Deterministic property & mutation fuzz tests for the Spid3r lexer, parser,
//! semantic analyzer, and `.matrixc` bytecode deserializer/validator.
//!
//! These run automatically as part of `cargo test` on stable Rust (complementing
//! the `cargo-fuzz` targets in `fuzz/fuzz_targets/`).

use std::sync::Arc;

use spid3r_lang::lexer;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::bytecode::{
    deserialize_vmfunc, serialize_vmfunc, validate_chunk, FORMAT_MAGIC, FORMAT_VERSION,
};
use spid3r_lang::vm::compile_program;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn gen_range(&mut self, max: usize) -> usize {
        if max == 0 {
            0
        } else {
            (self.next_u64() as usize) % max
        }
    }
}

#[test]
fn fuzz_lexer_never_panics_on_mutated_and_arbitrary_inputs() {
    let mut rng = Rng::new(0x501D_3E00_CAFE_BEEF);
    let seeds = [
        "fn main() { let x = 42; println(\"val={x}\"); return 0; }",
        "\"\n\\\"",
        "\"\\x\" \"unterminated",
        "// comment\r\n/* block /* nested */ */",
        "\u{FEFF}let a = 999999999999999999999999999999999.9;",
        "\"hello {1 + \"inner {2 + 3}\"}\"",
        "0x1234 0b1010 0o777 3.14159",
        "class A extends B { pub x; fn new(v) { self.x = v; } }",
    ];

    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789_+-*/%=!<>&|^~()[]{};:.,\"'\\ \t\r\n\x00\xff\xfe\xef\xbb\xbf";

    for round in 0..1500 {
        let base = seeds[round % seeds.len()].as_bytes();
        let mut buf = base.to_vec();
        let mutations = 1 + rng.gen_range(8);
        for _ in 0..mutations {
            let action = rng.gen_range(4);
            match action {
                0 if !buf.is_empty() => {
                    let idx = rng.gen_range(buf.len());
                    buf[idx] = alphabet[rng.gen_range(alphabet.len())];
                }
                1 => {
                    let idx = rng.gen_range(buf.len() + 1);
                    let b = alphabet[rng.gen_range(alphabet.len())];
                    buf.insert(idx, b);
                }
                2 if !buf.is_empty() => {
                    let idx = rng.gen_range(buf.len());
                    buf.remove(idx);
                }
                _ => {
                    let len = rng.gen_range(32);
                    buf.clear();
                    for _ in 0..len {
                        buf.push(alphabet[rng.gen_range(alphabet.len())]);
                    }
                }
            }
        }
        let src = String::from_utf8_lossy(&buf);
        let _ = lexer::Lexer::new(&src, Arc::from("<fuzz_lex>")).tokenize();
    }
}

#[test]
fn fuzz_parser_and_semantic_never_panic_on_mutated_token_streams() {
    let mut rng = Rng::new(0xA570_DA7A_1234_5678);
    let fragments = [
        "fn",
        "async",
        "let",
        "const",
        "mut",
        "if",
        "elif",
        "else",
        "while",
        "for",
        "in",
        "try",
        "catch",
        "finally",
        "throw",
        "return",
        "break",
        "continue",
        "class",
        "extends",
        "implements",
        "interface",
        "pub",
        "priv",
        "new",
        "spawn",
        "await",
        "import",
        "true",
        "false",
        "null",
        "and",
        "or",
        "not",
        "self",
        "x",
        "y",
        "main",
        "Foo",
        "0",
        "1",
        "3.14",
        "\"str\"",
        "\"a{x}b\"",
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        ";",
        ",",
        ".",
        ":",
        "=",
        "+=",
        "-=",
        "*=",
        "/=",
        "%=",
        "+",
        "-",
        "*",
        "/",
        "%",
        "==",
        "!=",
        "<",
        "<=",
        ">",
        ">=",
        "->",
    ];

    for _ in 0..1000 {
        let count = 1 + rng.gen_range(60);
        let mut src = String::with_capacity(count * 4);
        for _ in 0..count {
            let tok = fragments[rng.gen_range(fragments.len())];
            src.push_str(tok);
            if rng.gen_range(3) > 0 {
                src.push(' ');
            }
        }
        if let Ok(prog) = parser::parse_file(&src, Arc::from("<fuzz_parse>")) {
            let _ = semantic::check_file(&prog, false, "<fuzz_parse>");
        }
    }

    // Stress deep nesting of different syntactic forms around MAX_PARSE_DEPTH.
    for depth in [64usize, 96, 128, 200] {
        let nested_lists = format!(
            "fn main() {{ let x = {}{}; }}",
            "[".repeat(depth),
            "]".repeat(depth)
        );
        let _ = parser::parse_file(&nested_lists, Arc::from("<fuzz_depth>"));

        let nested_blocks = format!(
            "fn main() {{ {}{} }}",
            "{ ".repeat(depth),
            " }".repeat(depth)
        );
        let _ = parser::parse_file(&nested_blocks, Arc::from("<fuzz_depth>"));

        let nested_calls = format!(
            "fn main() {{ let x = {}1{}; }}",
            "f(".repeat(depth),
            ")".repeat(depth)
        );
        let _ = parser::parse_file(&nested_calls, Arc::from("<fuzz_depth>"));
    }
}

#[test]
fn fuzz_bytecode_deserializer_and_validator_never_panic_on_corrupted_chunks() {
    let sample_src = r#"
        class Box {
            public let v;
            public fn new(v) { self.v = v; }
            public fn get(self) { return self.v; }
        }
        fn helper(a, b) {
            let f = fn(x) { return a + b + x; };
            return f(10);
        }
        fn main() {
            try {
                let b = new Box(helper(1, 2));
                if b.get() > 0 {
                    return 0;
                }
            } catch (e) {
                return 1;
            } finally {
                let done = true;
            }
            return 0;
        }
    "#;
    let prog = parser::parse_file(sample_src, Arc::from("fuzz.matrix")).expect("parse sample");
    semantic::check_file(&prog, true, "fuzz.matrix").expect("check sample");
    let (func, _) = compile_program(&prog, "fuzz.matrix", &[]).expect("compile sample");
    let valid_bytes = serialize_vmfunc(&func);

    // Baseline must deserialize and validate cleanly.
    let decoded = deserialize_vmfunc(&valid_bytes).expect("baseline deserialize");
    validate_chunk(&decoded.chunk).expect("baseline validate");

    // 1. Every prefix truncation of valid_bytes must fail cleanly without panicking.
    for cut in 0..valid_bytes.len() {
        let slice = &valid_bytes[..cut];
        if let Ok(f) = deserialize_vmfunc(slice) {
            let _ = validate_chunk(&f.chunk);
        }
    }

    // 2. Single-byte and multi-byte random mutations across the serialized buffer.
    let mut rng = Rng::new(0xB17E_C0DE_9988_7766);
    for _ in 0..2000 {
        let mut mutated = valid_bytes.clone();
        let flips = 1 + rng.gen_range(6);
        for _ in 0..flips {
            let idx = 8 + rng.gen_range(mutated.len().saturating_sub(8));
            mutated[idx] = (rng.next_u64() & 0xFF) as u8;
        }
        if let Ok(f) = deserialize_vmfunc(&mutated) {
            let _ = validate_chunk(&f.chunk);
        }
    }

    // 3. Random synthetic buffers with valid header magic + version.
    for _ in 0..1000 {
        let mut buf = Vec::with_capacity(64);
        buf.extend_from_slice(FORMAT_MAGIC);
        buf.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        let len = rng.gen_range(128);
        for _ in 0..len {
            buf.push((rng.next_u64() & 0xFF) as u8);
        }
        if let Ok(f) = deserialize_vmfunc(&buf) {
            let _ = validate_chunk(&f.chunk);
        }
    }
}
