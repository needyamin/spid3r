//! Oracle parity for the VM's runtime fusions.
//!
//! The VM fuses a handful of opcode shapes at runtime (`GetLocal a; GetLocal b;
//! <op>; SetLocal d; Pop`, `GetLocal a; Constant k; <op>; SetLocal d; Pop`, and
//! the `Less` / `Mul` / `Mod` condition peeks). Those paths skip the generic
//! handlers, so they must reproduce them *exactly* — including the cases where
//! the generic handler raises an error instead of producing a value.
//!
//! The oracle is `--no-opt`, which turns the compile-time optimiser and the
//! runtime peeks off together, leaving plain bytecode dispatch. Every case here
//! is run three ways (default VM, `--no-opt` VM, tree) and must agree on stdout,
//! stderr and exit code byte for byte. A fused arm that quietly wraps where the
//! generic path errors — or that skips a fall-through — fails here rather than
//! in a user's program.
//!
//! Deliberately absent: any assertion on timing. This file is about meaning.

use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(engine: &str, extra: &[&str], path: &Path) -> Outcome {
    let o = Command::new(bin())
        .arg("run")
        .arg("--engine")
        .arg(engine)
        .args(extra)
        .arg(path.to_str().unwrap())
        .output()
        .expect("spawn spid3r");
    Outcome {
        code: o.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&o.stdout).to_string(),
        stderr: String::from_utf8_lossy(&o.stderr).to_string(),
    }
}

fn scratch(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("spid3r_perf_oracle_parity");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{name}.matrix"));
    std::fs::write(&path, src).expect("write case");
    path
}

/// Run one program on the fused path, the unfused path and the tree engine, and
/// require all three to agree on everything observable.
fn assert_parity(name: &str, src: &str) -> Outcome {
    let path = scratch(name, src);
    let fused = run("vm", &[], &path);
    let plain = run("vm", &["--no-opt"], &path);
    assert_eq!(
        fused.code, plain.code,
        "[{name}] exit code: fused {} vs --no-opt {}",
        fused.code, plain.code
    );
    assert_eq!(fused.stdout, plain.stdout, "[{name}] stdout vs --no-opt");
    assert_eq!(fused.stderr, plain.stderr, "[{name}] stderr vs --no-opt");

    let tree = run("tree", &[], &path);
    assert_eq!(fused.code, tree.code, "[{name}] exit code: vm vs tree");
    assert_eq!(fused.stdout, tree.stdout, "[{name}] stdout: vm vs tree");
    fused
}

fn assert_runtime_error(name: &str, src: &str, needle: &str) {
    let out = assert_parity(name, src);
    assert_ne!(out.code, 0, "[{name}] expected a non-zero exit");
    assert!(
        out.stderr.contains("error[runtime]"),
        "[{name}] expected a runtime error, got: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains(needle),
        "[{name}] expected {needle:?} in: {}",
        out.stderr
    );
}

// ---------------------------------------------------------------------------
// Happy paths: the fused arms must produce the value the generic handlers do.
// ---------------------------------------------------------------------------

/// `GetLocal a; GetLocal b; <op>; SetLocal a; Pop` for all five operators.
#[test]
fn local_op_local_arithmetic_matches_the_oracle() {
    let out = assert_parity(
        "local_local_ops",
        concat!(
            "fn main() {\n",
            "    let a = 100;\n",
            "    let b = 7;\n",
            "    a = a - b;\n",
            "    println(a);\n",
            "    a = a * b;\n",
            "    println(a);\n",
            "    a = a / b;\n",
            "    println(a);\n",
            "    a = a % b;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.replace("\r\n", "\n"), "93\n651\n93\n2\n");
}

/// `GetLocal a; Constant k; <op>; SetLocal a; Pop` for all five operators.
#[test]
fn local_op_const_arithmetic_matches_the_oracle() {
    let out = assert_parity(
        "local_const_ops",
        concat!(
            "fn main() {\n",
            "    let a = 100;\n",
            "    a = a - 7;\n",
            "    println(a);\n",
            "    a = a * 7;\n",
            "    println(a);\n",
            "    a = a / 7;\n",
            "    println(a);\n",
            "    a = a % 7;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.replace("\r\n", "\n"), "93\n651\n93\n2\n");
}

/// The string append arm lives in the same fused block as the integer ops; a
/// generalisation that broke it would show up here.
#[test]
fn string_append_still_uses_the_in_place_buffer() {
    let out = assert_parity(
        "string_append",
        concat!(
            "fn main() {\n",
            "    let s = \"\";\n",
            "    let i = 0;\n",
            "    while i < 200 {\n",
            "        s = s + \"ab\";\n",
            "        i = i + 1;\n",
            "    }\n",
            "    println(s.len());\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.replace("\r\n", "\n"), "400\n");
}

// ---------------------------------------------------------------------------
// Fall-through paths: checked_* returning None must reach the generic handler,
// which raises the exact same error. This is the property a fused arm is most
// likely to break, by wrapping where the interpreter errors.
// ---------------------------------------------------------------------------

#[test]
fn add_overflow_falls_through_to_the_generic_error() {
    assert_runtime_error(
        "add_overflow",
        concat!(
            "fn main() {\n",
            "    let a = 9223372036854775807;\n",
            "    let b = 1;\n",
            "    a = a + b;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "integer overflow",
    );
}

#[test]
fn sub_overflow_falls_through_to_the_generic_error() {
    assert_runtime_error(
        "sub_overflow",
        concat!(
            "fn main() {\n",
            "    let a = 0 - 9223372036854775807 - 1;\n",
            "    a = a - 1;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "integer overflow",
    );
}

#[test]
fn mul_overflow_falls_through_to_the_generic_error() {
    assert_runtime_error(
        "mul_overflow",
        concat!(
            "fn main() {\n",
            "    let a = 9223372036854775807;\n",
            "    a = a * 2;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "integer overflow",
    );
}

/// Same overflows, but with BOTH operands in locals, which selects the
/// `GetLocal; GetLocal; <op>; SetLocal; Pop` arm rather than the
/// `GetLocal; Constant; <op>; SetLocal; Pop` one. Without these cases a fused
/// arm that swapped `checked_sub` for `wrapping_sub` passed the whole file.
#[test]
fn overflow_through_the_local_operand_arm_also_falls_through() {
    assert_runtime_error(
        "sub_overflow_local_operand",
        concat!(
            "fn main() {\n",
            "    let a = 0 - 9223372036854775807 - 1;\n",
            "    let b = 1;\n",
            "    a = a - b;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "integer overflow",
    );
    assert_runtime_error(
        "mul_overflow_local_operand",
        concat!(
            "fn main() {\n",
            "    let a = 9223372036854775807;\n",
            "    let b = 2;\n",
            "    a = a * b;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "integer overflow",
    );
}

#[test]
fn division_by_zero_falls_through_to_the_generic_error() {
    assert_runtime_error(
        "div_by_zero_local",
        concat!(
            "fn main() {\n",
            "    let z = 0;\n",
            "    let a = 10;\n",
            "    a = a / z;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "division by zero",
    );
    assert_runtime_error(
        "div_by_zero_const",
        concat!(
            "fn main() {\n",
            "    let a = 10;\n",
            "    a = a / 0;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "division by zero",
    );
}

#[test]
fn modulo_by_zero_falls_through_to_the_generic_error() {
    assert_runtime_error(
        "mod_by_zero_local",
        concat!(
            "fn main() {\n",
            "    let z = 0;\n",
            "    let a = 10;\n",
            "    a = a % z;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "modulo by zero",
    );
    assert_runtime_error(
        "mod_by_zero_const",
        concat!(
            "fn main() {\n",
            "    let a = 10;\n",
            "    a = a % 0;\n",
            "    println(a);\n",
            "    return 0;\n",
            "}\n",
        ),
        "modulo by zero",
    );
}

/// `i64::MIN / -1` overflows; `i64::MIN % -1` is defined and is 0. The two must
/// not be treated the same: `%` uses `wrapping_rem`, `/` uses `checked_div`.
#[test]
fn i64_min_div_neg_one_overflows_like_the_generic_path() {
    assert_runtime_error(
        "min_div_neg_one",
        concat!(
            "fn main() {\n",
            "    let m = 0 - 9223372036854775807 - 1;\n",
            "    let d = 0 - 1;\n",
            "    let r = 0;\n",
            "    r = m / d;\n",
            "    println(r);\n",
            "    return 0;\n",
            "}\n",
        ),
        "integer overflow",
    );
}

#[test]
fn i64_min_mod_neg_one_wraps_like_the_generic_path() {
    let out = assert_parity(
        "min_mod_neg_one",
        concat!(
            "fn main() {\n",
            "    let m = 0 - 9223372036854775807 - 1;\n",
            "    let d = 0 - 1;\n",
            "    let r = 7;\n",
            "    r = m % d;\n",
            "    println(r);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.replace("\r\n", "\n"), "0\n");
}

/// One invoke site, two classes: the inline cache is monomorphic, so the
/// second class must miss, re-resolve and overwrite the slot. If the cache
/// ever returned the first class's method here, the sum would be wrong.
#[test]
fn one_invoke_site_with_two_classes_stays_correct() {
    let out = assert_parity(
        "two_classes_one_site",
        concat!(
            "class A {\n",
            "    public let v;\n",
            "    fn new () { self.v = 1; }\n",
            "    public fn get(self) -> int { return self.v; }\n",
            "}\n",
            "class B {\n",
            "    public let v;\n",
            "    fn new () { self.v = 2; }\n",
            "    public fn get(self) -> int { return self.v * 10; }\n",
            "}\n",
            "fn call_get(o) -> int { return o.get(); }\n",
            "fn main() {\n",
            "    let a = new A();\n",
            "    let b = new B();\n",
            "    let i = 0;\n",
            "    let t = 0;\n",
            "    while i < 100 {\n",
            "        t = t + call_get(a) + call_get(b);\n",
            "        i = i + 1;\n",
            "    }\n",
            "    println(t);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    // 100 * (A.get()=1 + B.get()=20) = 2100
    assert_eq!(out.stdout.replace("\r\n", "\n"), "2100\n");
}

/// A field write between calls must not be masked by anything the cache holds:
/// the cache stores the *method*, never the field value.
#[test]
fn cached_method_sees_fresh_field_values() {
    let out = assert_parity(
        "cache_sees_fresh_fields",
        concat!(
            "class C {\n",
            "    public let n;\n",
            "    fn new () { self.n = 0; }\n",
            "    public fn bump(self) -> int {\n",
            "        self.n = self.n + 1;\n",
            "        return self.n;\n",
            "    }\n",
            "}\n",
            "fn main() {\n",
            "    let c = new C();\n",
            "    let i = 0;\n",
            "    let last = 0;\n",
            "    while i < 1000 {\n",
            "        last = c.bump();\n",
            "        i = i + 1;\n",
            "    }\n",
            "    println(last);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    assert_eq!(out.stdout.replace("\r\n", "\n"), "1000\n");
}

// ---------------------------------------------------------------------------
// Observers: the fusions are gated off under --prof / --coverage / the
// debugger, so the same program must produce the same answer under them.
// ---------------------------------------------------------------------------

/// A captured upvalue disables every fusion in the frame (`captured` must be
/// empty for the peek to run), because a slot backed by a shared cell cannot
/// be read straight out of `slots`. The answer must not change either way.
#[test]
fn captured_upvalue_disables_fusion_without_changing_the_answer() {
    let out = assert_parity(
        "captured_upvalue",
        concat!(
            "fn main() {\n",
            "    let n = 500;\n",
            "    let total = 0;\n",
            "    let i = 0;\n",
            "    while i < n {\n",
            "        let add = fn(x) { return x + i; };\n",
            "        total = total + add(i);\n",
            "        i = i + 1;\n",
            "    }\n",
            "    println(total);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    // sum over i in [0, 500) of (i + i) = 2 * (499 * 500 / 2) = 249500
    assert_eq!(out.stdout.replace("\r\n", "\n"), "249500\n");
}

#[test]
fn instrumentation_flags_do_not_change_the_answer() {
    let src = concat!(
        "fn main() {\n",
        "    let a = 100;\n",
        "    let b = 7;\n",
        "    a = a - b;\n",
        "    a = a * b;\n",
        "    a = a / b;\n",
        "    a = a % b;\n",
        "    println(a);\n",
        "    return 0;\n",
        "}\n",
    );
    let path = scratch("instrumented_ops", src);
    let base = run("vm", &[], &path);
    for extra in [&["--prof"][..], &["--coverage"][..], &["--no-opt"][..]] {
        let other = run("vm", extra, &path);
        assert_eq!(base.code, other.code, "exit code with {extra:?}");
        assert_eq!(base.stdout, other.stdout, "stdout with {extra:?}");
    }
}

// ---------------------------------------------------------------------------
// Instance layout: the VM's field slots and the tree's field map must agree.
// ---------------------------------------------------------------------------

/// A subclass re-declaring a parent's field keeps one value per name, and all
/// three execution paths agree on it.
///
/// The checkers reject a duplicate field within one class but not across the
/// inheritance chain, so the old per-instance field map ended up with one
/// entry (parents inserted first, the child's insert overwriting in place)
/// while the display order — the full declaration chain — contained the name
/// twice. Every engine printed `B {x: 10, x: 10}`: one field reported as two,
/// a defect rather than a semantic choice anyone could depend on.
///
/// VM instances now lay fields out as slots named by the class (a shadowed
/// name keeps its outermost slot, where the old map kept the value), and the
/// tree display dedupes the declaration order by first occurrence.
/// `--no-opt` is part of the gate on purpose: the first draft of the slot
/// table was inverted exactly there — the optimized run fused the `FieldSet`
/// away and masked it, the unoptimized one took the out-of-bounds index.
#[test]
fn shadowed_instance_field_renders_once_on_every_path() {
    let out = assert_parity(
        "shadowed_field",
        concat!(
            "class A {\n",
            "    public let x;\n",
            "    public let y;\n",
            "    fn new () { self.x = 1; self.y = 2; }\n",
            "}\n",
            "class B extends A {\n",
            "    public let x;\n",
            "    fn new () { super(); self.x = 10; }\n",
            "}\n",
            "fn main() {\n",
            "    let b = new B();\n",
            "    println(b);\n",
            "    println(b.x);\n",
            "    println(b.y);\n",
            "    return 0;\n",
            "}\n",
        ),
    );
    assert_eq!(out.code, 0);
    // One value per name, the shadowed `x` at its outermost position, and the
    // non-shadowed fields (`y`, assigned by A's ctor) unchanged: the same
    // answers the old field-map layout produced.
    assert_eq!(
        out.stdout.replace("\r\n", "\n"),
        "B {x: 10, y: 2}\n10\n2\n",
        "shadowed field output differs"
    );
}

// ---------------------------------------------------------------------------
// Map inline-cache invalidation corpus (W2.3).
//
// `cached_map_field_get` caches an INDEX (never a value) per constant-name dot
// site, validated on every hit by map identity, name, `MapData::generation`
// and the key at the cached index. The corpus pins the mutations that must
// invalidate it — and, through the expected outputs below, the ones that must
// not. The gate for this corpus: removing the generation check from the hit
// validation must fail at least one case (the negative cache — a missing key
// has no index to re-check, so generation is the only thing that invalidates
// it); it was verified by hand when this corpus landed and is re-verifiable by
// deleting `|| c.generation != guard.generation` from `cached_map_field_get`
// and running this module.
// ---------------------------------------------------------------------------

/// Assert three-engine parity plus the exact expected stdout (CRLF-normalized).
fn assert_map_cache_case(name: &str, body: &str, expected: &str) {
    let out = assert_parity(name, body);
    assert_eq!(out.code, 0, "[{name}] exit code {}", out.code);
    assert_eq!(
        out.stdout.replace("\r\n", "\n"),
        expected,
        "[{name}] stdout differs"
    );
}

/// A key inserted after a site cached its absence must be visible on the next
/// read through that same site.
///
/// This is THE generation-dependent case: the missing-key result is cached as
/// `index: None`, and a missing key has no cached index whose entry could be
/// re-checked, so the generation is the only invalidation signal there is.
/// Insertions bump the generation (`set_key`); without the check the site
/// would answer `null` forever.
#[test]
fn cache_negative_result_invalidated_by_insertion() {
    assert_map_cache_case(
        "cache_neg_insert",
        concat!(
            "fn peek(m) { return m?.k; }\n",
            "fn main() {\n",
            "    let m = map();\n",
            "    println(peek(m));\n", // null — site caches the absence
            "    m[\"k\"] = 5;\n",     // insertion bumps the generation
            "    println(peek(m));\n", // must re-resolve to 5
            "    m[\"k\"] = 7;\n",     // in-place rebind, fresh read
            "    println(peek(m));\n", // 7
            "    return 0;\n",
            "}\n",
        ),
        "null\n5\n7\n",
    );
}

/// Mutating some other key must not make a still-missing key crash or leak a
/// stale index: the site re-resolves and the absence is re-cached.
#[test]
fn cache_negative_result_survives_unrelated_mutation() {
    assert_map_cache_case(
        "cache_neg_unrelated",
        concat!(
            "fn peek(m) { return m?.k; }\n",
            "fn main() {\n",
            "    let m = map();\n",
            "    println(peek(m));\n",   // null
            "    m[\"other\"] = 1;\n",   // bump: k still absent
            "    println(peek(m));\n",   // null again, freshly re-resolved
            "    return 0;\n",
            "}\n",
        ),
        "null\nnull\n",
    );
}

/// Removing a key shifts later entries down and reinserting appends it at the
/// end; the cache must follow through all three shapes (positive → negative →
/// positive at a different index).
///
/// With the generation check removed this also fails: after `remove("k")` the
/// site re-resolves to a *negative* cache entry, and the later insertion of
/// `"k"` then reads as the stale null forever.
#[test]
fn cache_tracks_removal_and_reinsertion() {
    assert_map_cache_case(
        "cache_remove_reinsert",
        concat!(
            "fn peek(m) { return m?.k; }\n",
            "fn main() {\n",
            "    let m = map();\n",
            "    m[\"a\"] = 1; m[\"b\"] = 2; m[\"k\"] = 3; m[\"d\"] = 4;\n",
            "    println(peek(m));\n", // 3 — k at index 2
            "    m.remove(\"k\");\n",  // reindex shifts `d` 3 -> 2
            "    println(peek(m));\n", // null — key check on the stale index
            "    m[\"k\"] = 30;\n",    // appended at index 3
            "    println(peek(m));\n", // 30 — new index, fresh read
            "    m.remove(\"d\");\n",  // reindex again
            "    println(peek(m));\n", // 30 — re-resolve finds the new slot
            "    return 0;\n",
            "}\n",
        ),
        "3\nnull\n30\n30\n",
    );
}

/// Two maps alternating through one constant-name site must never answer with
/// the other map's value: the entry is validated by map identity on every hit.
#[test]
fn cache_distinguishes_maps_by_identity() {
    assert_map_cache_case(
        "cache_two_maps",
        concat!(
            "fn peek(m) { return m.k; }\n",
            "fn main() {\n",
            "    let a = map(); a[\"k\"] = 1;\n",
            "    let b = map(); b[\"k\"] = 2;\n",
            "    let i = 0;\n",
            "    while i < 3 {\n",
            "        println(peek(a));\n",
            "        println(peek(b));\n",
            "        i += 1;\n",
            "    }\n",
            "    return 0;\n",
            "}\n",
        ),
        "1\n2\n1\n2\n1\n2\n",
    );
}

/// In-place rebinding must be visible immediately: the cache stores an index,
/// the value is read fresh under the lock on every hit.
#[test]
fn cache_reads_values_fresh_on_every_hit() {
    assert_map_cache_case(
        "cache_fresh_value",
        concat!(
            "fn peek(m) { return m.k; }\n",
            "fn main() {\n",
            "    let m = map();\n",
            "    m[\"k\"] = 1;\n",
            "    let i = 0;\n",
            "    while i < 3 {\n",
            "        println(peek(m));\n",   // 1, then 0, then 10
            "        m[\"k\"] = i * 10;\n", // rebind in place
            "        i += 1;\n",
            "        println(peek(m));\n",   // 0, 10, 20
            "    }\n",
            "    return 0;\n",
            "}\n",
        ),
        "1\n0\n0\n10\n10\n20\n",
    );
}

/// `clear` empties and reindexes (bumping the generation), so a positive cache
/// goes negative and the rebuild must be visible — the mirror of the insertion
/// case, and it fails without the generation check for the same reason.
#[test]
fn cache_tracks_clear_and_rebuild() {
    assert_map_cache_case(
        "cache_clear_rebuild",
        concat!(
            "fn peek(m) { return m?.k; }\n",
            "fn main() {\n",
            "    let m = map();\n",
            "    m[\"k\"] = 1;\n",
            "    println(peek(m));\n", // 1
            "    m.clear();\n",
            "    println(peek(m));\n", // null
            "    m[\"k\"] = 2;\n",
            "    println(peek(m));\n", // 2 — back at index 0, freshly resolved
            "    return 0;\n",
            "}\n",
        ),
        "1\nnull\n2\n",
    );
}
