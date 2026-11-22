//! Phase 0 integrity guard (P0-1): prevents adding program-specific bytecode
//! fingerprints or unbounded multi-instruction byte-peeks to `src/vm/mod.rs`.
//!
//! Single-expression / single-statement lookaheads (`code[ip + N]` /
//! `code.get(ip + N)` with `N <= 14`) are permitted for general bytecode fusion.
//! Deep lookaheads (`N > 14`) are strictly confined to the two audited legacy
//! blocks below, neither of which may grow; Region 2 (trial-division) is
//! scheduled for full deletion in Phase 3 once general superinstructions land.

use std::fs;
use std::path::Path;

const MAX_GENERAL_LOOKAHEAD: usize = 14;

const REGION1_MARKER: &str = "// VM-1 + VM-2: Batched unboxed `i64`";
const REGION1_WINDOW_LINES: usize = 80;
const REGION1_MAX_OFFSET: usize = 24;
const REGION1_MAX_COUNT: usize = 10;

const REGION2_MARKER: &str = "// VM-1: Batched unboxed `i64` trial-division";
const REGION2_WINDOW_LINES: usize = 130;
const REGION2_MAX_OFFSET: usize = 44;
const REGION2_MAX_COUNT: usize = 33;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LookaheadAudit {
    region1_line: Option<usize>,
    region2_line: Option<usize>,
    region1_count: usize,
    region2_count: usize,
    total_deep_peeks: usize,
    max_general_offset: usize,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn strip_comments_preserve_lines(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut block_depth = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if block_depth > 0 {
            if b == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
                block_depth += 1;
                out.extend_from_slice(b"  ");
                i += 2;
                continue;
            }
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                block_depth -= 1;
                out.extend_from_slice(b"  ");
                i += 2;
                continue;
            }
            out.push(if b == b'\n' { b'\n' } else { b' ' });
            i += 1;
            continue;
        }
        if in_string {
            out.push(if b == b'\n' { b'\n' } else { b' ' });
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if b == b'/' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'/' {
                while i < bytes.len() && bytes[i] != b'\n' {
                    out.push(b' ');
                    i += 1;
                }
                continue;
            }
            if bytes[i + 1] == b'*' {
                block_depth = 1;
                out.extend_from_slice(b"  ");
                i += 2;
                continue;
            }
        }
        if b == b'"' {
            in_string = true;
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(b);
        i += 1;
    }

    String::from_utf8(out).expect("ASCII comment stripping preserves valid UTF-8")
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn check_vm_lookaheads(src: &str) -> Result<LookaheadAudit, Vec<String>> {
    let mut violations = Vec::new();
    let mut region1_lines = Vec::new();
    let mut region2_lines = Vec::new();

    for (idx, line) in src.lines().enumerate() {
        let line_no = idx + 1;
        if line.contains(REGION1_MARKER) {
            region1_lines.push(line_no);
        }
        if line.contains(REGION2_MARKER) {
            region2_lines.push(line_no);
        }
    }

    if region1_lines.len() > 1 {
        violations.push(format!(
            "duplicate Region 1 allowlist markers at lines {:?}",
            region1_lines
        ));
    }
    if region2_lines.len() > 1 {
        violations.push(format!(
            "duplicate Region 2 allowlist markers at lines {:?}",
            region2_lines
        ));
    }

    let r1_start = region1_lines.first().copied();
    let r2_start = region2_lines.first().copied();
    let stripped = strip_comments_preserve_lines(src);
    let bytes = stripped.as_bytes();

    let mut line_no = 1usize;
    let mut i = 0usize;
    let mut region1_count = 0usize;
    let mut region2_count = 0usize;
    let mut max_general_offset = 0usize;

    while i < bytes.len() {
        if bytes[i] == b'\n' {
            line_no += 1;
            i += 1;
            continue;
        }
        let starts_code = bytes[i..].starts_with(b"code")
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
            && (i + 4 >= bytes.len() || !is_ident_byte(bytes[i + 4]));
        if !starts_code {
            i += 1;
            continue;
        }

        let match_line = line_no;
        let mut p = skip_ws(bytes, i + 4);
        if p < bytes.len() && bytes[p] == b'[' {
            p += 1;
        } else if bytes[p..].starts_with(b".") {
            p = skip_ws(bytes, p + 1);
            if !bytes[p..].starts_with(b"get") {
                i += 4;
                continue;
            }
            p = skip_ws(bytes, p + 3);
            if p >= bytes.len() || bytes[p] != b'(' {
                i += 4;
                continue;
            }
            p += 1;
        } else {
            i += 4;
            continue;
        }

        p = skip_ws(bytes, p);
        if !bytes[p..].starts_with(b"ip") || (p + 2 < bytes.len() && is_ident_byte(bytes[p + 2])) {
            i += 4;
            continue;
        }
        p = skip_ws(bytes, p + 2);
        if p >= bytes.len() || bytes[p] != b'+' {
            i += 4;
            continue;
        }
        p = skip_ws(bytes, p + 1);
        let num_start = p;
        while p < bytes.len() && bytes[p].is_ascii_digit() {
            p += 1;
        }
        if p == num_start || (p < bytes.len() && is_ident_byte(bytes[p])) {
            i += 4;
            continue;
        }

        let offset: usize = std::str::from_utf8(&bytes[num_start..p])
            .unwrap()
            .parse()
            .unwrap_or(usize::MAX);

        if offset <= MAX_GENERAL_LOOKAHEAD {
            max_general_offset = max_general_offset.max(offset);
        } else if let Some(s1) =
            r1_start.filter(|&s| match_line >= s && match_line <= s + REGION1_WINDOW_LINES)
        {
            if offset > REGION1_MAX_OFFSET {
                violations.push(format!(
                    "line {match_line}: Region 1 deep lookahead `ip + {offset}` exceeds max offset {REGION1_MAX_OFFSET} (region starts at line {s1})"
                ));
            }
            region1_count += 1;
            if region1_count > REGION1_MAX_COUNT {
                violations.push(format!(
                    "line {match_line}: Region 1 deep lookahead `ip + {offset}` exceeds cap ({region1_count} > {REGION1_MAX_COUNT})"
                ));
            }
        } else if let Some(s2) =
            r2_start.filter(|&s| match_line >= s && match_line <= s + REGION2_WINDOW_LINES)
        {
            if offset > REGION2_MAX_OFFSET {
                violations.push(format!(
                    "line {match_line}: Region 2 deep lookahead `ip + {offset}` exceeds max offset {REGION2_MAX_OFFSET} (region starts at line {s2})"
                ));
            }
            region2_count += 1;
            if region2_count > REGION2_MAX_COUNT {
                violations.push(format!(
                    "line {match_line}: Region 2 deep lookahead `ip + {offset}` exceeds cap ({region2_count} > {REGION2_MAX_COUNT})"
                ));
            }
        } else {
            violations.push(format!(
                "line {match_line}: unauthorized deep bytecode lookahead `ip + {offset}` (> {MAX_GENERAL_LOOKAHEAD}); add a general compile-time superinstruction in src/vm/opt.rs instead"
            ));
        }

        i += 4;
    }

    if !violations.is_empty() {
        return Err(violations);
    }

    Ok(LookaheadAudit {
        region1_line: r1_start,
        region2_line: r2_start,
        region1_count,
        region2_count,
        total_deep_peeks: region1_count + region2_count,
        max_general_offset,
    })
}

#[test]
fn vm_has_no_unauthorized_bytecode_fingerprints() {
    let vm_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/vm/mod.rs");
    let src = fs::read_to_string(&vm_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", vm_path.display()));
    let audit = check_vm_lookaheads(&src).unwrap_or_else(|errs| {
        panic!(
            "VM bytecode fingerprint guard failed on {}:\n  - {}",
            vm_path.display(),
            errs.join("\n  - ")
        )
    });
    assert!(
        audit.region1_count <= REGION1_MAX_COUNT,
        "Region 1 deep peek count grew: {} > {}",
        audit.region1_count,
        REGION1_MAX_COUNT
    );
    assert!(
        audit.region2_count <= REGION2_MAX_COUNT,
        "Region 2 deep peek count grew: {} > {}",
        audit.region2_count,
        REGION2_MAX_COUNT
    );
    assert!(
        audit.total_deep_peeks <= REGION1_MAX_COUNT + REGION2_MAX_COUNT,
        "total deep peeks grew: {}",
        audit.total_deep_peeks
    );
}

#[test]
fn guard_rejects_unauthorized_deep_peek() {
    let bad = r#"
        fn dispatch(code: &[u8], ip: usize) {
            let ok = code[ip + 14];
            let bad1 = code[ip + 18];
            let bad2 = code.get(
                ip + 25
            );
            let ignored_in_comment = 0; // code[ip + 99]
            let ignored_in_str = "code[ip + 88]";
            let _ = (ok, bad1, bad2, ignored_in_comment, ignored_in_str);
        }
    "#;
    let errs = check_vm_lookaheads(bad).expect_err("guard must reject ip+18 and ip+25");
    assert_eq!(errs.len(), 2, "unexpected errors: {errs:?}");
    assert!(errs[0].contains("ip + 18"), "{}", errs[0]);
    assert!(errs[1].contains("ip + 25"), "{}", errs[1]);
}

#[test]
fn guard_rejects_expanding_allowlisted_region() {
    let mut lines = vec![format!("fn f(code: &[u8], ip: usize) {{ {REGION1_MARKER}")];
    for idx in 0..=REGION1_MAX_COUNT {
        lines.push(format!("    let _ = code[ip + {}];", 15 + (idx % 8)));
    }
    lines.push("}".to_string());
    let src = lines.join("\n");
    let errs = check_vm_lookaheads(&src).expect_err("guard must reject 11th Region 1 deep peek");
    assert!(
        errs.iter().any(|e| e.contains("exceeds cap")),
        "expected cap violation, got: {errs:?}"
    );
}
