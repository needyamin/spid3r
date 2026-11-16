//! Phase 11 P-5 lazy-initialization & behavioral parity tests.
//!
//! Each integration test binary runs in its own fresh process, so `plain_run_*`
//! directly proves that running a program that calls no builtins / time zones
//! initializes neither the native lookup table nor `tzdata.bin`.

use std::path::PathBuf;
use std::sync::Arc;

use spid3r_lang::embed::{Capability, Engine};
use spid3r_lang::interpreter::Interp;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::stdlib::{builtin_table_initialized, tz};
use spid3r_lang::vm::Vm;

#[test]
fn plain_run_keeps_native_and_tz_tables_uninitialized_then_resolves_lazily() {
    assert!(
        !builtin_table_initialized(),
        "builtin table must start uninitialized"
    );
    assert!(
        !tz::is_initialized(),
        "tzdata table must start uninitialized"
    );

    // 1. Plain program invoking zero builtins on Tree and VM leaves both tables uninitialized.
    let src_plain = "fn add(a: int, b: int) -> int { return a + b; }\nfn main() -> int { return add(20, 22); }\n";
    let prog_plain = parser::parse_file(src_plain, Arc::from("<plain>")).unwrap();
    semantic::check_file(&prog_plain, true, "<plain>").unwrap();

    let interp_plain = Interp::new(Arc::from("<plain>"), Vec::<PathBuf>::new());
    assert_eq!(interp_plain.run_program(&prog_plain).unwrap(), 42);
    assert!(
        !builtin_table_initialized(),
        "tree plain run must not eagerly initialize builtin native table"
    );
    assert!(
        !tz::is_initialized(),
        "tree plain run must not parse tzdata"
    );

    let mut vm_plain = Vm::new(Arc::from("<plain>"), Vec::<PathBuf>::new());
    assert_eq!(vm_plain.run_program(&prog_plain).unwrap(), 42);
    assert!(
        !builtin_table_initialized(),
        "VM plain run must not eagerly initialize builtin native table"
    );
    assert!(!tz::is_initialized(), "VM plain run must not parse tzdata");

    #[cfg(feature = "native")]
    {
        spid3r_lang::native::rt::rt_init();
        assert!(
            !builtin_table_initialized(),
            "AOT rt_init must not eagerly initialize builtin native table"
        );
        assert!(!tz::is_initialized(), "AOT rt_init must not parse tzdata");
        let exists_obj = spid3r_lang::native::rt::spid3r_global_exists(c"Object".as_ptr());
        assert_eq!(exists_obj, 1);
        assert!(
            !builtin_table_initialized(),
            "resolving Object in AOT runtime must still keep builtin table uninitialized"
        );
    }

    // 2. Program touching `len` + `unicode_casefold` initializes builtin table lazily, still leaves tzdata untouched.
    let src_str = r#"
fn main() -> int {
    let f = unicode_casefold("ÄÖÜ");
    if len(f) > 0 { return len("spid3r"); }
    return 1;
}
"#;
    let prog_str = parser::parse_file(src_str, Arc::from("<str>")).unwrap();
    semantic::check_file(&prog_str, true, "<str>").unwrap();

    let mut vm_str = Vm::new(Arc::from("<str>"), Vec::<PathBuf>::new());
    assert_eq!(vm_str.run_program(&prog_str).unwrap(), 6);
    assert!(
        builtin_table_initialized(),
        "calling builtins must initialize builtin table on demand"
    );
    assert!(
        !tz::is_initialized(),
        "unicode/len calls must still keep tzdata uninitialized"
    );

    let interp_str = Interp::new(Arc::from("<str>"), Vec::<PathBuf>::new());
    assert_eq!(interp_str.run_program(&prog_str).unwrap(), 6);
    assert!(!tz::is_initialized());

    // 3. Builtin shadowing + capability denial stay identical under lazy resolution.
    let shadow_src = r#"
fn main() -> int {
    let len = 99;
    if len == 99 and abs(-7) == 7 { return 0; }
    return 1;
}
"#;
    let prog_shadow = parser::parse_file(shadow_src, Arc::from("<shadow>")).unwrap();
    semantic::check_file(&prog_shadow, true, "<shadow>").unwrap();
    assert_eq!(
        Interp::new(Arc::from("<shadow>"), Vec::new())
            .run_program(&prog_shadow)
            .unwrap(),
        0
    );
    assert_eq!(
        Vm::new(Arc::from("<shadow>"), Vec::new())
            .run_program(&prog_shadow)
            .unwrap(),
        0
    );

    let engine = Engine::new().deny(Capability::FileSystem);
    let out = engine
        .run(
            "deny_fs",
            r#"fn main() {
                try { read_file("Cargo.toml"); return 1; } catch e { println("ok"); }
                return 0;
            }"#,
        )
        .expect("sandboxed script executes");
    assert_eq!(out.exit_code, 0);
    assert_eq!(out.output, "ok\n");

    // 4. Touching tz initializes tzdata lazily.
    let tz_src = r#"
fn main() -> int {
    let info = tz_info("UTC", 0).unwrap();
    if info["abbr"] == "UTC" { return 0; }
    return 1;
}
"#;
    let prog_tz = parser::parse_file(tz_src, Arc::from("<tz>")).unwrap();
    semantic::check_file(&prog_tz, true, "<tz>").unwrap();
    assert_eq!(
        Vm::new(Arc::from("<tz>"), Vec::new())
            .run_program(&prog_tz)
            .unwrap(),
        0
    );
    assert!(
        tz::is_initialized(),
        "tz_info must lazily initialize tzdata"
    );

    #[cfg(feature = "native")]
    {
        let exists_abs = spid3r_lang::native::rt::spid3r_global_exists(c"abs".as_ptr());
        assert_eq!(exists_abs, 1);
    }
}
