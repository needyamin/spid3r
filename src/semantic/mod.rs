//! Semantic pass: name resolution, scope rules, privacy, interface
//! conformance, arity of statically known callees, a `main` requirement, and
//! optional *gradual* type checking (`spid3r check` enforces `: Type`
//! annotations; unannotated names stay dynamically typed).

mod ty;
use ty::Ty;

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diagnostics::{Diagnostic, Lint, Phase, Span, SpidResult};

/// Signature of a declared method, retained so call sites and `return`s inside
/// the method body can be type-checked.
#[derive(Clone)]
struct MethodInfo {
    vis: Vis,
    /// Parameter types, including `self` at index 0.
    params: Vec<Option<TypeExpr>>,
    /// Parameter names, including `self` at index 0.
    param_names: Vec<String>,
    ret: Option<TypeExpr>,
    /// Smallest accepted argument count *including* `self` (`self` is never
    /// defaulted, so it always contributes). Call sites pass one fewer.
    min_arity: usize,
}

impl MethodInfo {
    fn arity(&self) -> usize {
        self.params.len()
    }
}

/// Signature of a top-level (or nested) declared function.
#[derive(Clone)]
struct FuncInfo {
    arity: usize,
    /// Smallest accepted argument count (constant defaults are trailing-only,
    /// parser-enforced). `== arity` when no parameter has a default.
    min_arity: usize,
    params: Vec<Option<TypeExpr>>,
    /// Parameter names in declaration order (for named arguments).
    param_names: Vec<String>,
    ret: Option<TypeExpr>,
}

#[derive(Clone)]
struct ClassInfo {
    parent: Option<String>,
    interfaces: Vec<String>,
    fields: HashMap<String, (Vis, Option<TypeExpr>)>,
    methods: HashMap<String, MethodInfo>,
    /// `abstract class` - cannot be instantiated.
    is_abstract: bool,
    /// Methods declared `abstract fn m();` here; a concrete subclass must
    /// provide them.
    abstracts: Vec<String>,
}

#[derive(Default)]
struct VarInfo {
    is_const: bool,
    class: Option<String>,
    /// Static type of the binding. `None` means dynamically typed (`Any`).
    ty: Option<Ty>,
    /// True when the binding entered scope through an `import`. Used to
    /// diagnose local declarations that shadow an imported binding (and
    /// imports that would shadow an existing local in the REPL).
    is_import: bool,
    /// What kind of binding this is, for the unused-* lints' wording.
    kind: BindKind,
    /// Set when the name is read. `pop_scope` reports the ones that never are.
    used: bool,
    /// Where the binding was declared, for the warning's snippet. Filled by
    /// `declare`, which already receives the span.
    decl: Option<Span>,
    /// For an import **alias** (`import "m" as mu`): the names the module
    /// exports as `public const`. A namespace is a read-only view of the
    /// module, so these must not be rebound through the alias
    /// (`mu["VERSION"] = 1`).
    import_consts: Option<Vec<String>>,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum BindKind {
    #[default]
    Local,
    Param,
    Import,
}

struct Checker {
    classes: HashMap<String, ClassInfo>,
    interfaces: HashMap<String, HashMap<String, (usize, Option<TypeExpr>)>>,
    funcs: HashMap<String, FuncInfo>,
    scopes: Vec<HashMap<String, VarInfo>>,
    loop_depth: usize,
    fn_depth: usize,
    in_async: bool,
    in_main_fn: bool,
    current_class: Option<String>,
    current_ret: Option<Ty>,
    is_main: bool,
    source_file: String,
    /// Top-level `fn` names pass 1 pre-declared into scope[0]. They sit in
    /// scope before any `import` is processed even though they come later in
    /// source order, so the import handler uses this set to tell a later-in-
    /// this-chunk local fn from an earlier one (REPL `known` names).
    predeclared: HashSet<String>,
    /// Enum name -> variant names, for the exhaustiveness check.
    enums: HashMap<String, Vec<String>>,
    /// Variant name -> its enum, so a match can tell which enum it covers.
    variant_enum: HashMap<String, String>,
    /// Externally-bound names (embedding API): host-registered natives and
    /// globals. They resolve as dynamic (`Any`) bindings — known but unchecked.
    externals: HashSet<String>,
    /// Warnings produced by the lints. Errors short-circuit the checker, so
    /// these are only collected on a successful check.
    warnings: Vec<Diagnostic>,
    /// Extra module search roots for embedded runs (host `add_module_dir`).
    module_dirs: Vec<std::path::PathBuf>,
    /// Erased generic parameter names, innermost last. A declaration's own
    /// `type_params` are pushed while its body (and its annotations) are
    /// checked, so `-> T` resolves as a type name instead of "unknown type".
    type_params: Vec<Vec<String>>,
    /// Unexported declarations in imported modules (name -> (module_display, kind))
    unexported_module_names: HashMap<String, (String, &'static str)>,
    /// Depth of `gui_on` handler arguments being checked. A `gui_on` handler
    /// MUST take an (event, payload) signature it may never read, so the
    /// unused-parameter lint stays silent for closures appearing directly as
    /// its handler argument (B3). Prefixing with `_` still works and remains
    /// the documented way for other callback-taking APIs.
    gui_handler_depth: usize,
}

pub fn check(program: &Program, is_main: bool) -> SpidResult<()> {
    check_file(program, is_main, "<memory>")
}

pub fn check_file(program: &Program, is_main: bool, source_file: &str) -> SpidResult<()> {
    check_file_with(program, is_main, source_file, &Externals::default())
}

/// Like [`check_file`], but also returns the lint warnings.
///
/// Errors still short-circuit: a file that does not check produces no warnings.
/// Teach a checker that a host plugin's functions exist.
///
/// A plugin native is merged into `stdlib::native_defs()` (or, for a WASM
/// plugin, installed as a closure) and is callable from a script, so the
/// checker must not reject the call as an unknown name. Read once per check
/// rather than on every identifier lookup, so `known()` stays a set membership
/// test.
fn seed_plugin_externals(c: &mut Checker) {
    for f in crate::plugin::registered_functions() {
        c.externals.insert(f.name.to_string());
    }
    // WASM plugin functions are closures rather than `fn` pointers, but they are
    // natives in exactly the same sense and must pass the same check.
    for (name, _) in crate::plugin::registered_closures() {
        c.externals.insert(name);
    }
}

/// Callers that only care about validity keep using `check_file`.
pub fn check_file_with_warnings(
    program: &Program,
    is_main: bool,
    source_file: &str,
) -> Result<Vec<Diagnostic>, Diagnostic> {
    let mut c = Checker::new(is_main, source_file);
    seed_plugin_externals(&mut c);
    c.check_program(program)?;
    // Warnings surface when a scope closes, which is inside-out; sort them so
    // they read top-to-bottom like the source.
    c.warnings.sort_by_key(|w| {
        w.span
            .as_ref()
            .map(|s| (s.line, s.col))
            .unwrap_or((u32::MAX, u32::MAX))
    });
    Ok(c.warnings)
}

/// Externally-visible configuration for embedded runs: host-bound names
/// (dynamic, unchecked) and extra module search roots.
#[derive(Default, Clone)]
pub struct Externals {
    /// Host-registered natives and globals (embedding API).
    pub names: HashSet<String>,
    /// Module search roots appended after the standard ones.
    pub module_dirs: Vec<std::path::PathBuf>,
}

/// Like [`check_file`], but with EXTERNALLY-BOUND names (embedding API):
/// host-registered natives and globals. They resolve as dynamically typed
/// (`Any`) bindings — callable, assignable, unchecked — exactly like the
/// stdlib natives the checker treats as builtin functions. Scripts may
/// shadow them with `let` (which overwrites the host binding at runtime).
/// `externals.module_dirs` extends the import search path so scripts can
/// import modules the host ships alongside it.
pub fn check_file_with(
    program: &Program,
    is_main: bool,
    source_file: &str,
    externals: &Externals,
) -> SpidResult<()> {
    let mut c = Checker::new(is_main, source_file);
    c.externals = externals.names.clone();
    seed_plugin_externals(&mut c);
    c.module_dirs = externals.module_dirs.clone();
    c.check_program(program).map(|_| ())
}

/// Names a chunk of code binds at its top level. The REPL uses this to carry
/// bindings from one line to the next.
pub fn declared_names(program: &Program) -> Vec<String> {
    let mut out = Vec::new();
    for s in &program.stmts {
        match s {
            Stmt::Fn(f) => {
                if let Some(n) = &f.name {
                    out.push(n.clone());
                }
            }
            Stmt::Class(c) => out.push(c.name.clone()),
            Stmt::Interface(i) => out.push(i.name.clone()),
            Stmt::Let { name, .. } | Stmt::Const { name, .. } => out.push(name.clone()),
            _ => {}
        }
    }
    out
}

/// Check one REPL chunk. `known` holds the names bound by earlier lines, so a
/// later line can refer to them.
pub fn check_repl(program: &Program, known: &[String]) -> SpidResult<()> {
    check_repl_in(program, known, "<repl>")
}

/// Like [`check_repl`], but with an explicit source file so the import
/// resolver can locate modules relative to it (used to exercise REPL-style
/// chunk checking against real module files).
pub fn check_repl_in(program: &Program, known: &[String], source_file: &str) -> SpidResult<()> {
    let mut c = Checker::new(false, source_file);
    for n in known {
        c.scopes[0].insert(
            n.clone(),
            VarInfo {
                is_const: false,
                class: None,
                ty: None,
                is_import: false,
                ..Default::default()
            },
        );
    }
    c.check_program(program)
}

impl Checker {
    /// A type-mismatch diagnostic with the suggestion users actually need:
    /// annotations are compile-time only, so the fix is either `any` or a
    /// conversion.
    fn mismatch(&self, span: &Span, msg: String) -> Diagnostic {
        self.err(span, msg).with_help(
            "annotations are checked before the program runs; use `any` to opt out, \
             or convert explicitly with string(x) / int(x) / float(x)",
        )
    }

    fn new(is_main: bool, source_file: &str) -> Checker {
        Checker {
            classes: HashMap::new(),
            interfaces: HashMap::new(),
            funcs: HashMap::new(),
            scopes: vec![HashMap::new()],
            loop_depth: 0,
            fn_depth: 0,
            in_async: false,
            in_main_fn: false,
            current_class: None,
            current_ret: None,
            is_main,
            source_file: source_file.to_string(),
            predeclared: HashSet::new(),
            warnings: Vec::new(),
            enums: HashMap::new(),
            variant_enum: HashMap::new(),
            externals: HashSet::new(),
            module_dirs: Vec::new(),
            type_params: Vec::new(),
            unexported_module_names: HashMap::new(),
            gui_handler_depth: 0,
        }
    }
}

const BUILTIN_CONSTRUCTORS: &[&str] = &[
    "list",
    "vec",
    "stack",
    "queue",
    "deque",
    "linked_list",
    "singly_linked_list",
    "doubly_linked_list",
    "map",
    "set",
    "tuple",
    "binary_tree",
    "bst",
    "heap",
    "priority_queue",
    "graph",
    "hash_table",
    "trie",
    "matrix",
    "matrix_from_list",
    "vector",
    "bytes",
    "ring_buffer",
    "bag",
    "ref",
    "channel",
    "mutex",
    "some",
    "none",
    "ok",
    "err",
];

const BUILTIN_FUNCTIONS: &[&str] = &[
    "print",
    "println",
    "assert",
    "assert_eq",
    "weak",
    "deref",
    "eprint",
    "read_line",
    "read_file",
    "write_file",
    "write_file_atomic",
    "file_exists",
    "file_delete",
    "remove_file",
    "rename",
    "copy_file",
    "list_dir",
    "is_dir",
    "is_file",
    "file_size",
    "file_mtime",
    "cwd",
    "chdir",
    "exe_path",
    "env_set",
    "path_join",
    "basename",
    "dirname",
    "path_ext",
    "is_ok",
    "is_err",
    "dir_create",
    "dir_remove",
    "sleep_ms",
    "now",
    "env",
    "args",
    "gui_args",
    "spawn_process",
    "run_payload",
    "run_payload_strict",
    "exec_capture",
    "yield",
    "task_count",
    "len",
    "abs",
    "sqrt",
    "pow",
    "min",
    "max",
    "clamp",
    "floor",
    "ceil",
    "round",
    "trunc",
    "sign",
    "exp",
    "ln",
    "log10",
    "log2",
    "sin",
    "cos",
    "tan",
    "asin",
    "acos",
    "atan",
    "is_nan",
    "is_inf",
    "atan2",
    "hypot",
    "float_or",
    "nan",
    "inf",
    "random_float",
    "random_normal",
    "sha256",
    "hmac_sha256",
    "pbkdf2_sha256",
    "secure_compare",
    "random_bytes",
    "chr",
    "ord",
    "crc32",
    "adler32",
    "deflate",
    "inflate",
    "gzip_compress",
    "gunzip",
    "zlib_compress",
    "unzlib",
    "zip_create",
    "zip_read",
    "zip_list",
    "msgpack_encode",
    "msgpack_decode",
    "cbor_encode",
    "cbor_decode",
    "pb_encode",
    "pb_decode",
    "fft",
    "ifft",
    "scrypt",
    "redis_close",
    "redis_command",
    "redis_connect",
    "pg_close",
    "pg_exec",
    "pg_query",
    "pg_connect",
    "mysql_close",
    "mysql_exec",
    "mysql_query",
    "mysql_connect",
    "argon2",
    "bcrypt_hash",
    "bcrypt_verify",
    "png_decode",
    "png_encode",
    "wav_decode",
    "wav_encode",
    "pdf_write",
    "pdf_escape",
    "base64_encode",
    "base64_decode",
    "base64_url_encode",
    "base64_url_decode",
    "hex_encode",
    "hex_decode",
    "base32_encode",
    "base32_decode",
    "int",
    "float",
    "string",
    "bool",
    // desugaring targets for the bitwise operators
    "bit_and",
    "bit_or",
    "bit_xor",
    "shl",
    "shr",
    "random_int",
    "json_parse",
    "json_write",
    "type_of",
    "is_failure",
    "gc_collect",
    "gc_live_cells",
    // networking (src/stdlib/net.rs)
    "tcp_listen",
    "tcp_accept",
    "tcp_accept_timeout",
    "tcp_connect",
    "tcp_read",
    "tcp_read_exact",
    "tcp_write_bytes",
    "tcp_write",
    "tcp_close",
    "tcp_local_addr",
    "tcp_peer_addr",
    "tcp_set_timeout",
    "tcp_poll",
    "tcp_recv_to_file",
    "tcp_send_file",
    "ws_connect",
    "ws_accept",
    "ws_send",
    "ws_recv",
    "ws_close",
    "udp_bind",
    "udp_local_addr",
    "udp_send_to",
    "udp_recv_from",
    "udp_join_multicast",
    "udp_leave_multicast",
    "udp_set_multicast_ttl",
    "udp_set_broadcast",
    "udp_close",
    "http_read_request",
    "http_write_response",
    "http_fetch",
    "__http_fetch",
    "multipart_parse",
    "__multipart_parse",
    "multipart_build",
    "__multipart_build",
    "shutdown_requested",
    "request_shutdown",
    "reset_shutdown",
    "url_decode",
    "url_encode",
    // binary I/O and npy
    "read_bytes",
    "bytes_concat",
    "write_bytes",
    "save_npy",
    "load_npy",
    // csv
    "read_csv",
    "parse_csv",
    "write_csv",
    // regex
    "regex_match",
    "regex_find",
    "regex_find_all",
    "regex_replace",
    // datetime
    "unix_time",
    "now_ms",
    "elapsed_ms",
    "unix_ms",
    "datetime_parts",
    "datetime_from_parts",
    "format_datetime",
    // time zones (src/stdlib/tz.rs) and Unicode (src/stdlib/unicode.rs)
    "tz_names",
    "tz_info",
    "tz_parts",
    "tz_from_parts",
    "tz_format",
    "tz_version",
    "unicode_normalize",
    "unicode_is_normalized",
    "unicode_casefold",
    "unicode_strip_marks",
    "unicode_combining_class",
    "unicode_version",
    // sqlite database (src/stdlib/db.rs)
    "db_open",
];

/// GUI natives (`spid3r gui`). Only known when the toolchain is built with the
/// `gui` feature: without it, `gui_eval` & co. stay unknown names and a GUI
/// program fails the semantic check with a clear diagnostic.
#[cfg(feature = "gui")]
const GUI_BUILTIN_FUNCTIONS: &[&str] = &[
    "gui_eval",
    "gui_set_html",
    "gui_on",
    "gui_emit",
    "gui_title",
];

/// GUI natives known to this build (empty without the `gui` feature).
#[cfg(feature = "gui")]
pub fn gui_builtins() -> &'static [&'static str] {
    GUI_BUILTIN_FUNCTIONS
}

#[cfg(not(feature = "gui"))]
pub fn gui_builtins() -> &'static [&'static str] {
    &[]
}

/// Return type of a handful of builtin functions, used to give call sites a
/// precise static type. Anything not listed returns `Any`.
fn builtin_return_type(name: &str) -> Option<Ty> {
    Some(match name {
        "int" | "len" | "abs" | "random_int" | "now" | "now_ms" | "elapsed_ms" | "unix_time"
        | "unix_ms" => Ty::Int,
        // Every 1-arg scalar math builtin (plus the 2-/3-arg float helpers
        // and the random float generators) answers with a float.
        "float" | "sqrt" | "pow" | "clamp" | "floor" | "ceil" | "round" | "trunc" | "sign"
        | "exp" | "ln" | "log10" | "log2" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan"
        | "atan2" | "hypot" | "float_or" | "nan" | "inf" | "random_float" | "random_normal" => {
            Ty::Float
        }
        "is_nan" | "is_inf" | "is_ok" | "is_err" => Ty::Bool,
        "secure_compare" => Ty::Bool,
        "string" => Ty::Str,
        "bit_and" | "bit_or" | "bit_xor" | "shl" | "shr" => Ty::Int,
        "sha256" | "hmac_sha256" | "pbkdf2_sha256" => Ty::Str,
        "crc32" | "adler32" => Ty::Int,
        "scrypt" => Ty::Str,
        "argon2" => Ty::Str,
        "bcrypt_hash" => Ty::Str,
        "bcrypt_verify" => Ty::Bool,
        "chr" => Ty::Str,
        "ord" => Ty::Int,
        "base64_encode" | "base64_url_encode" => Ty::Str,
        "bool" => Ty::Bool,
        "type_of" => Ty::Str,
        "is_failure" => Ty::Bool,
        _ => return None,
    })
}

impl Checker {
    /// Declare the names a pattern binds inside the arm's scope.
    fn declare_pattern(&mut self, p: &Pattern) -> SpidResult<()> {
        match p {
            Pattern::Wild | Pattern::Literal(_) => Ok(()),
            Pattern::Bind(name) => {
                self.declare(
                    name,
                    &p.span(),
                    VarInfo {
                        is_const: false,
                        class: None,
                        ty: None,
                        is_import: false,
                        ..Default::default()
                    },
                )?;
                Ok(())
            }
            Pattern::Variant { name, args, span } => {
                if !self.variant_enum.contains_key(name) && !self.known(name) {
                    return Err(self.err(
                        span,
                        format!("unknown enum variant '{}' in a match pattern", name),
                    ));
                }
                for a in args {
                    self.declare_pattern(a)?;
                }
                Ok(())
            }
        }
    }

    /// Report a match that cannot succeed for every variant of the enum it
    /// covers.
    ///
    /// A dynamic language rarely knows the scrutinee's static type, so the
    /// enum is inferred from the variants the arms mention: if every variant
    /// pattern belongs to one enum and there is no catch-all arm, that enum is
    /// expected to be covered. Anything ambiguous (literals only, variants from
    /// two enums, a `_` or binding arm) is left alone — the runtime reports an
    /// unmatched value instead.
    fn check_exhaustive(&self, arms: &[MatchArm], span: &Span) -> SpidResult<()> {
        let mut owner: Option<&str> = None;
        let mut covered: Vec<&str> = Vec::new();
        for arm in arms {
            match &arm.pattern {
                // A catch-all (or a guard on one) makes the match total.
                Pattern::Wild | Pattern::Bind(_) => return Ok(()),
                Pattern::Literal(_) => return Ok(()),
                Pattern::Variant { name, .. } => {
                    let Some(e) = self.variant_enum.get(name) else {
                        return Ok(());
                    };
                    match owner {
                        None => owner = Some(e.as_str()),
                        Some(prev) if prev != e.as_str() => return Ok(()),
                        _ => {}
                    }
                    covered.push(name.as_str());
                }
            }
        }
        let Some(e) = owner else {
            return Ok(());
        };
        let Some(all) = self.enums.get(e) else {
            return Ok(());
        };
        let missing: Vec<&str> = all
            .iter()
            .map(|v| v.as_str())
            .filter(|v| !covered.contains(v))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        Err(self.err(
            span,
            format!(
                "this match is not exhaustive for enum '{}': missing {} (add an arm or `_`)",
                e,
                missing
                    .iter()
                    .map(|m| format!("'{}'", m))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ))
    }
}

/// Does a block end in something that produces a value?
///
/// A trailing expression does; so does `return`/`throw`, which leave the block
/// before the value is ever read.
fn block_has_value(b: &Block) -> bool {
    match b.stmts.last() {
        Some(Stmt::Expr(_)) | Some(Stmt::Return { .. }) | Some(Stmt::Throw { .. }) => true,
        // A nested block that itself has a value.
        Some(Stmt::Block(inner)) => block_has_value(inner),
        _ => false,
    }
}

/// The type of a block used as a value (its last expression).
fn block_value_type(c: &Checker, b: &Block) -> Ty {
    match b.stmts.last() {
        Some(Stmt::Expr(e)) => c.type_of(e),
        Some(Stmt::Block(inner)) => block_value_type(c, inner),
        _ => Ty::Any,
    }
}

/// The collection a constructor really produces.
///
/// `some`/`none` are `option` constructors and `ok`/`err` are `result`
/// constructors. Normalising the name here is what makes an `option<T>` (or
/// `result<T>`) annotation usable at all — the annotation resolves to
/// `Coll("option")` while `some(5)` used to produce `Coll("some")`, so every
/// annotated binding was a type error.
fn coll_kind(name: &str) -> &str {
    match name {
        "some" | "none" => "option",
        "ok" | "err" => "result",
        other => other,
    }
}

/// The type that can hold both `a` and `b`.
fn common_type(a: Ty, b: Ty) -> Ty {
    if a == b {
        return a;
    }
    match (a, b) {
        (Ty::Any, _) | (_, Ty::Any) => Ty::Any,
        (Ty::Null, other) | (other, Ty::Null) => other,
        (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) => Ty::Float,
        _ => Ty::Any,
    }
}

impl Checker {
    fn err(&self, span: &Span, msg: impl Into<String>) -> Diagnostic {
        Diagnostic::at(Phase::Semantic, span.clone(), msg)
    }

    fn unknown_name(&self, span: &Span, name: &str) -> Diagnostic {
        if let Some((mod_display, kind)) = self.unexported_module_names.get(name) {
            self.err(
                span,
                format!(
                    "{} is declared in {} but not exported — mark it `public {}`",
                    name, mod_display, kind
                ),
            )
        } else {
            self.err(span, format!("unknown name '{}'", name))
        }
    }

    fn check_program(&mut self, program: &Program) -> SpidResult<()> {
        // pass 1: collect top-level names
        let mut seen = HashSet::new();
        for s in &program.stmts {
            match s {
                // An enum's variants are constructors: `Circle(1)` is a call of
                // a function that `crate::lower` emits. Registering them here
                // means a use above the declaration still type-checks.
                Stmt::Enum(e) => {
                    let mut names = Vec::new();
                    for v in &e.variants {
                        if !seen.insert(v.name.clone()) {
                            return Err(
                                self.err(&v.span, format!("'{}' is declared twice", v.name))
                            );
                        }
                        names.push(v.name.clone());
                        self.variant_enum.insert(v.name.clone(), e.name.clone());
                        self.funcs.insert(
                            v.name.clone(),
                            FuncInfo {
                                arity: v.fields.len(),
                                min_arity: v.fields.len(),
                                params: v.fields.iter().map(|_| None).collect(),
                                param_names: v.fields.clone(),
                                ret: None,
                            },
                        );
                    }
                    self.enums.insert(e.name.clone(), names);
                }
                Stmt::Fn(f) => {
                    let name = f.name.clone().unwrap_or_default();
                    if name.is_empty() {
                        return Err(self.err(&f.span, "a top-level function needs a name"));
                    }
                    if !seen.insert(name.clone()) {
                        return Err(self.err(&f.span, format!("'{}' is declared twice", name)));
                    }
                    if f.params.iter().any(|p| p.name == "self") {
                        return Err(self.err(&f.span, "self is only allowed in methods"));
                    }
                    // A10: `main` is called with no arguments and its result is
                    // the process exit code, so a signature the runtime cannot
                    // honor must fail `check` instead of failing at startup.
                    if self.is_main && name == "main" {
                        if !f.params.is_empty() {
                            return Err(self.err(
                                &f.span,
                                format!(
                                    "fn main must take no parameters (found {}); the runtime \
                                     calls it with none",
                                    f.params.len()
                                ),
                            ));
                        }
                        if let Some(ret) = &f.ret {
                            // Report an unknown type name the standard way
                            // first ("str" vs "string"), then require int.
                            self.check_type_name(&ret.name, &ret.span)?;
                            if !ret.name.eq_ignore_ascii_case("int") {
                                return Err(self.err(
                                    &f.span,
                                    format!(
                                        "fn main must return int (the exit code) or nothing, \
                                         not {}",
                                        ret.name
                                    ),
                                ));
                            }
                        }
                    }
                    self.funcs.insert(
                        name.clone(),
                        FuncInfo {
                            arity: f.params.len(),
                            min_arity: f.min_arity(),
                            params: f.params.iter().map(|p| p.ty.clone()).collect(),
                            param_names: f.params.iter().map(|p| p.name.clone()).collect(),
                            ret: f.ret.clone(),
                        },
                    );
                    // F12 (REPL direction): an earlier chunk may have imported
                    // this name — catch the collision at the declaration, not
                    // silently at the overwrite. File-mode pass 1 runs before
                    // any import is processed, so scope[0] holds nothing here.
                    if let Some(v) = self.scopes.last().and_then(|s| s.get(&name)) {
                        if v.is_import {
                            return Err(self.err(
                                &f.span,
                                format!("local '{}' shadows an imported binding", name),
                            ));
                        }
                    }
                    self.declare(
                        &name,
                        &f.span,
                        VarInfo {
                            is_const: false,
                            class: None,
                            // F5: the declaration name carries the function's
                            // signature so `let g = f; g(...)` is checked.
                            ty: Some(self.fn_sig_type(f)),
                            is_import: false,
                            ..Default::default()
                        },
                    )?;
                    self.predeclared.insert(name);
                }
                Stmt::Class(c) => {
                    if !seen.insert(c.name.clone()) {
                        return Err(
                            self.err(&c.span, format!("class '{}' is declared twice", c.name))
                        );
                    }
                }
                Stmt::Interface(i) => {
                    if !seen.insert(i.name.clone()) {
                        return Err(
                            self.err(&i.span, format!("interface '{}' is declared twice", i.name))
                        );
                    }
                }
                Stmt::Let { name, span, .. } | Stmt::Const { name, span, .. }
                    if !seen.insert(name.clone()) =>
                {
                    return Err(self.err(span, format!("'{}' is declared twice", name)));
                }
                _ => {}
            }
        }
        // class registry
        for s in &program.stmts {
            if let Stmt::Class(c) = s {
                let mut fields = HashMap::new();
                for f in &c.fields {
                    if fields
                        .insert(f.name.clone(), (f.vis, f.ty.clone()))
                        .is_some()
                    {
                        return Err(self.err(&f.span, format!("field '{}' declared twice", f.name)));
                    }
                }
                let mut methods = HashMap::new();
                for m in &c.methods {
                    let name = m.name.clone().unwrap_or_default();
                    if methods
                        .insert(
                            name.clone(),
                            MethodInfo {
                                vis: m.vis,
                                params: m.params.iter().map(|p| p.ty.clone()).collect(),
                                param_names: m.params.iter().map(|p| p.name.clone()).collect(),
                                ret: m.ret.clone(),
                                min_arity: m.min_arity(),
                            },
                        )
                        .is_some()
                    {
                        return Err(self.err(&m.span, format!("method '{}' declared twice", name)));
                    }
                    let is_ctor = name == "new";
                    if !is_ctor && m.params.first().map(|p| p.name.as_str()) != Some("self") {
                        return Err(self.err(
                            &m.span,
                            format!("method '{}' must take self explicitly", name),
                        ));
                    }
                }
                if let Some(parent) = &c.parent {
                    if parent != "Object" && !seen.contains(parent) {
                        return Err(self.err(
                            &c.span,
                            format!("parent class '{}' is not declared", parent),
                        ));
                    }
                }
                self.classes.insert(
                    c.name.clone(),
                    ClassInfo {
                        parent: c.parent.clone(),
                        interfaces: c.interfaces.clone(),
                        fields,
                        methods,
                        is_abstract: c.is_abstract,
                        abstracts: c.abstracts.clone(),
                    },
                );
            }
        }

        // Abstract contracts: every concrete class must implement the
        // abstract methods it inherits (walking up through intermediate
        // classes, which may implement some of them).
        let mut class_spans: HashMap<String, Span> = HashMap::new();
        for s in &program.stmts {
            if let Stmt::Class(c) = s {
                class_spans.insert(c.name.clone(), c.span.clone());
            }
        }
        let names: Vec<String> = self.classes.keys().cloned().collect();
        for name in names {
            let Some(ci) = self.classes.get(&name).cloned() else {
                continue;
            };
            if ci.is_abstract {
                continue;
            }
            // Required: (declaring class, method) pairs from the ancestors.
            let mut required: Vec<(String, String)> = Vec::new();
            let mut cur = ci.parent.clone();
            while let Some(p) = cur {
                let Some(pc) = self.classes.get(&p).cloned() else {
                    break;
                };
                for a in &pc.abstracts {
                    required.push((p.clone(), a.clone()));
                }
                cur = pc.parent.clone();
            }
            for (owner, method) in required {
                let mut found = false;
                let mut walk = Some(name.clone());
                while let Some(cn) = walk {
                    if cn == owner {
                        break;
                    }
                    let Some(cc) = self.classes.get(&cn).cloned() else {
                        break;
                    };
                    // A concrete definition (not the abstract declaration).
                    if cc.methods.contains_key(&method) && !cc.abstracts.contains(&method) {
                        found = true;
                        break;
                    }
                    walk = cc.parent.clone();
                }
                if !found {
                    let sp = class_spans
                        .get(&name)
                        .cloned()
                        .unwrap_or_else(|| Span::new(std::sync::Arc::from("<abstract>"), 0, 0, 0));
                    return Err(self.err(
                        &sp,
                        format!(
                            "class '{}' must implement the abstract method '{}' from '{}'",
                            name, method, owner
                        ),
                    ));
                }
            }
        }
        for s in &program.stmts {
            if let Stmt::Interface(i) = s {
                let mut methods = HashMap::new();
                for (name, params, ret, span) in &i.methods {
                    if params.first().map(|p| p.name.as_str()) != Some("self") {
                        return Err(
                            self.err(span, format!("interface method '{}' must take self", name))
                        );
                    }
                    methods.insert(name.clone(), (params.len(), ret.clone()));
                }
                self.interfaces.insert(i.name.clone(), methods);
            }
        }
        // implements conformance
        for s in &program.stmts {
            if let Stmt::Class(c) = s {
                for iface in &c.interfaces {
                    let methods = match self.interfaces.get(iface) {
                        Some(m) => m,
                        None => {
                            return Err(
                                self.err(&c.span, format!("interface '{}' is not declared", iface))
                            )
                        }
                    };
                    for (mname, (arity, iface_ret)) in methods {
                        match self.classes.get(&c.name).unwrap().methods.get(mname) {
                            Some(mi) => {
                                if mi.vis != Vis::Public {
                                    return Err(self.err(
                                        &c.span,
                                        format!(
                                            "method '{}' must be public to satisfy interface '{}'",
                                            mname, iface
                                        ),
                                    ));
                                }
                                if mi.arity() != *arity {
                                    return Err(self.err(
                                        &c.span,
                                        format!(
                                            "method '{}' has arity {} but interface '{}' wants {}",
                                            mname,
                                            mi.arity(),
                                            iface,
                                            arity
                                        ),
                                    ));
                                }
                                if let Some(expected_ret) = iface_ret {
                                    let exp_ty = self.resolve_type(expected_ret);
                                    if let Some(act_ret) = &mi.ret {
                                        let act_ty = self.resolve_type(act_ret);
                                        if !self.assignable(&act_ty, &exp_ty) {
                                            return Err(self.err(
                                                &c.span,
                                                format!(
                                                    "method '{}' returns {} but interface '{}' expects {}",
                                                    mname,
                                                    act_ty.name(),
                                                    iface,
                                                    exp_ty.name()
                                                ),
                                            ));
                                        }
                                    }
                                }
                            }
                            None => {
                                return Err(self.err(
                                    &c.span,
                                    format!(
                                        "class '{}' does not implement '{}' required by interface '{}'",
                                        c.name, mname, iface
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
        }
        // main
        if self.is_main && !self.funcs.contains_key("main") {
            return Err(Diagnostic::new(
                Phase::Semantic,
                "a program run by `spid3r run` needs fn main()",
            ));
        }

        // pass 2: walk bodies
        for s in &program.stmts {
            if self.is_main {
                if let Stmt::Expr(Expr::Call { callee, span, .. }) = s {
                    if let Expr::Ident(name, _) = &**callee {
                        if name == "main" {
                            self.warnings.push(Diagnostic::warning(
                                Lint::UnreachableCode,
                                span.clone(),
                                "explicit call to 'main': main() is invoked automatically; remove this call",
                            ));
                        }
                    }
                }
            }
            self.check_stmt(s)?;
        }
        self.pop_scope();
        Ok(())
    }

    fn declare(&mut self, name: &str, span: &Span, mut info: VarInfo) -> SpidResult<()> {
        // The lint needs the declaration site; every caller already has it.
        info.decl = Some(span.clone());
        let scope = self.scopes.last_mut().unwrap();
        if scope.contains_key(name) {
            return Err(self.err(
                span,
                format!("'{}' is already declared in this scope", name),
            ));
        }
        scope.insert(name.to_string(), info);
        Ok(())
    }

    fn lookup(&self, name: &str) -> Option<&VarInfo> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        None
    }

    /// The `public const` exports of the module an expression names, when the
    /// expression is an import **alias** (`import "m" as mu`).
    fn alias_const_exports(&self, expr: &Expr) -> Option<&[String]> {
        let Expr::Ident(name, _) = expr else {
            return None;
        };
        self.lookup(name).and_then(|v| v.import_consts.as_deref())
    }

    /// A string literal's text, when the expression is one.
    fn string_literal(expr: &Expr) -> Option<&str> {
        match expr {
            Expr::Str(s, _) => Some(s.as_str()),
            _ => None,
        }
    }

    fn known(&self, name: &str) -> bool {
        // Host-registered names (embedding API) resolve dynamically.
        self.externals.contains(name)
            || self.lookup(name).is_some()
            || self.funcs.contains_key(name)
            || self.classes.contains_key(name)
            || self.interfaces.contains_key(name)
            || name == "Object"
            || name == "none"
            || BUILTIN_CONSTRUCTORS.contains(&name)
            || BUILTIN_FUNCTIONS.contains(&name)
            || gui_builtins().contains(&name)
    }

    /// Field visibility and declaring class through the inheritance chain.
    fn field_decl(&self, class: &str, field: &str) -> Option<(Vis, String)> {
        let mut cur = Some(class.to_string());
        let mut seen = HashSet::new();
        while let Some(c) = cur {
            if !seen.insert(c.clone()) {
                return None; // cycle guard
            }
            let info = self.classes.get(&c)?;
            if let Some((vis, _)) = info.fields.get(field) {
                return Some((*vis, c));
            }
            cur = info.parent.clone();
        }
        None
    }

    #[allow(dead_code)]
    fn field_vis(&self, class: &str, field: &str) -> Option<Vis> {
        self.field_decl(class, field).map(|(v, _)| v)
    }

    /// Validate a `super` use against the class table.
    ///
    /// `method` is `None` for a parent-constructor call (`super(args)`) and
    /// `Some(name)` for a parent-method call (`super.name(args)`). The
    /// receiver is implicit in both: the current instance is the `self`.
    fn check_super(
        &self,
        span: &Span,
        method: Option<&str>,
        args: &[Expr],
    ) -> Result<(), Diagnostic> {
        let Some(class) = self.current_class.as_deref() else {
            return Err(self.err(span, "super is only allowed inside a class method"));
        };
        let Some(parent) = self.classes.get(class).and_then(|ci| ci.parent.clone()) else {
            return Err(self.err(
                span,
                format!(
                    "class '{}' has no parent class: super is not available here",
                    class
                ),
            ));
        };
        self.check_super_in(&parent, span, method, args)
    }

    /// Validate a `super` use against an explicit parent class.
    fn check_super_in(
        &self,
        parent: &str,
        span: &Span,
        method: Option<&str>,
        args: &[Expr],
    ) -> Result<(), Diagnostic> {
        let parent = parent.to_string();
        let parent = parent.as_str();
        let argc = args.len();
        match method {
            Some(m) => {
                let Some((sig, decl_cls)) = self.method_decl(parent, m) else {
                    return Err(self.err(span, format!("class '{}' has no method '{}'", parent, m)));
                };
                if sig.vis == Vis::Private {
                    return Err(self.err(
                        span,
                        format!("method '{}' of class '{}' is private", m, decl_cls),
                    ));
                }
                if Self::has_named_args(args) {
                    let names: &[String] =
                        if sig.param_names.first().map(|s| s.as_str()) == Some("self") {
                            &sig.param_names[1..]
                        } else {
                            &sig.param_names
                        };
                    let required = sig.min_arity.saturating_sub(1);
                    let label = format!("super.{}", m);
                    self.check_named_args(args, names, required, &label, span)?;
                } else if argc + 1 < sig.min_arity || argc + 1 > sig.arity() {
                    // A method takes `self` explicitly; the call does not.
                    let want = if sig.min_arity == sig.arity() {
                        format!("{} argument(s)", sig.arity() - 1)
                    } else {
                        format!("{} to {} argument(s)", sig.min_arity - 1, sig.arity() - 1)
                    };
                    return Err(self.err(
                        span,
                        format!("method {}::{} expects {}, got {}", parent, m, want, argc),
                    ));
                }
            }
            None => {
                let Some(sig) = self.method_sig(parent, "new") else {
                    return Err(self.err(span, format!("class '{}' has no constructor", parent)));
                };
                if Self::has_named_args(args) {
                    self.check_named_args(args, &sig.param_names, sig.min_arity, "super", span)?;
                } else if argc < sig.min_arity || argc > sig.arity() {
                    // A constructor takes no `self` parameter.
                    let want = if sig.min_arity == sig.arity() {
                        format!("{} argument(s)", sig.arity())
                    } else {
                        format!("{} to {} argument(s)", sig.min_arity, sig.arity())
                    };
                    return Err(self.err(
                        span,
                        format!("constructor {}::new expects {}, got {}", parent, want, argc),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Method (visibility, arity) through the inheritance chain.
    /// Validate a call whose arguments contain named arguments
    /// (`name: value`). The plain count-based arity check is skipped for such
    /// calls; this is the stricter replacement. `names` lists the callee's
    /// parameters in declaration order and `required` how many of the leading
    /// parameters have no default.
    fn check_named_args(
        &self,
        args: &[Expr],
        names: &[String],
        required: usize,
        label: &str,
        span: &Span,
    ) -> SpidResult<()> {
        let mut seen_named = false;
        let mut provided = vec![false; names.len()];
        let mut positional = 0usize;
        for a in args {
            match a {
                Expr::NamedArg {
                    name, span: nsp, ..
                } => {
                    seen_named = true;
                    match names.iter().position(|p| p == name) {
                        Some(i) => {
                            if provided[i] {
                                return Err(
                                    self.err(nsp, format!("duplicate named argument '{}'", name))
                                );
                            }
                            provided[i] = true;
                        }
                        None => {
                            return Err(self.err(
                                nsp,
                                format!("unknown parameter '{}' in the call to {}", name, label),
                            ));
                        }
                    }
                }
                other => {
                    if seen_named {
                        return Err(self.err(
                            other.span(),
                            "a positional argument cannot follow a named one",
                        ));
                    }
                    if positional < names.len() {
                        provided[positional] = true;
                    }
                    positional += 1;
                }
            }
        }
        if positional > names.len() {
            return Err(self.err(
                span,
                format!(
                    "too many positional arguments for {} (at most {})",
                    label,
                    names.len()
                ),
            ));
        }
        for (i, name) in names.iter().enumerate() {
            if !provided[i] && i < required {
                return Err(self.err(
                    span,
                    format!(
                        "missing required parameter '{}' in the call to {}",
                        name, label
                    ),
                ));
            }
        }
        Ok(())
    }

    /// Check one call argument: a named argument checks its VALUE (the name
    /// is validated by `check_named_args` against the callee signature).
    fn check_arg(&mut self, a: &Expr) -> SpidResult<()> {
        match a {
            Expr::NamedArg { value, .. } => self.check_expr(value),
            other => self.check_expr(other),
        }
    }

    /// True when any argument is a named argument.
    fn has_named_args(args: &[Expr]) -> bool {
        args.iter().any(|a| matches!(a, Expr::NamedArg { .. }))
    }

    /// Constant default values only (`= 10`, `= "x"`, `= true`, `= null`,
    /// unary minus over a numeric literal). Expression defaults that reference
    /// earlier parameters are a documented future step — the engines fill
    /// missing arguments from baked constants today.
    fn is_const_default(e: &Expr) -> bool {
        match e {
            Expr::Int(..) | Expr::Float(..) | Expr::Str(..) | Expr::Bool(..) | Expr::Null(_) => {
                true
            }
            Expr::Unary {
                op: UnOp::Neg,
                expr,
                ..
            } => {
                matches!(**expr, Expr::Int(..) | Expr::Float(..))
            }
            _ => false,
        }
    }

    /// Full method signature and declaring class through the inheritance chain.
    fn method_decl(&self, class: &str, method: &str) -> Option<(&MethodInfo, String)> {
        let mut cur = Some(class.to_string());
        let mut seen = HashSet::new();
        while let Some(c) = cur {
            if !seen.insert(c.clone()) {
                return None; // cycle guard
            }
            let ci = self.classes.get(&c)?;
            if let Some(m) = ci.methods.get(method) {
                return Some((m, c));
            }
            cur = ci.parent.clone();
        }
        None
    }

    /// Full method signature through the inheritance chain.
    fn method_sig(&self, class: &str, method: &str) -> Option<&MethodInfo> {
        self.method_decl(class, method).map(|(m, _)| m)
    }

    #[allow(dead_code)]
    fn method_info(&self, class: &str, method: &str) -> Option<(Vis, usize, usize)> {
        self.method_sig(class, method)
            .map(|m| (m.vis, m.arity(), m.min_arity))
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Record that `name` was read, so the unused-* lints stay quiet.
    fn mark_used(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(v) = scope.get_mut(name) {
                v.used = true;
                return;
            }
        }
    }

    fn pop_scope(&mut self) {
        let scope = match self.scopes.pop() {
            Some(s) => s,
            None => return,
        };
        // Scope 0 is the file's top level: a private global may be used from
        // another module, and a public one is an export, so only unused
        // *imports* are worth reporting there.
        let top_level = self.scopes.is_empty();
        for (name, info) in scope {
            if info.used || name.starts_with('_') || info.kind == BindKind::Import && !top_level {
                continue;
            }
            let Some(span) = info.decl.clone() else {
                continue;
            };
            let (lint, what) = match info.kind {
                BindKind::Param => (Lint::UnusedParameter, "parameter"),
                BindKind::Import => (Lint::UnusedImport, "import"),
                BindKind::Local => (Lint::UnusedVariable, "variable"),
            };
            if top_level && info.kind != BindKind::Import {
                continue;
            }
            self.warnings.push(Diagnostic::warning(
                lint,
                span,
                format!("unused {} '{}'", what, name),
            ));
        }
    }

    fn check_block(&mut self, b: &Block) -> SpidResult<()> {
        self.push_scope();
        let r = self.check_stmts(&b.stmts);
        self.pop_scope();
        r
    }

    fn check_stmts(&mut self, stmts: &[Stmt]) -> SpidResult<()> {
        let mut terminated = false;
        // One report per block: a run of dead statements is one finding.
        let mut dead_reported = false;
        for s in stmts {
            if terminated && !dead_reported {
                self.warnings.push(Diagnostic::warning(
                    Lint::UnreachableCode,
                    s.span().clone(),
                    "unreachable code: this statement follows a `return`, `throw`, \
                     `break`, or `continue`",
                ));
                dead_reported = true;
            }
            self.check_stmt(s)?;
            terminated = matches!(
                s,
                Stmt::Return { .. }
                    | Stmt::Throw { .. }
                    | Stmt::Break { .. }
                    | Stmt::Continue { .. }
            );
        }
        Ok(())
    }

    fn check_stmt(&mut self, s: &Stmt) -> SpidResult<()> {
        match s {
            Stmt::Yield { value, span } => {
                if self.in_main_fn {
                    return Err(self.err(span, "fn main cannot be a generator"));
                }
                self.check_expr(value)
            }
            // Variants were registered in pass 1; a variant declaration carries
            // no body to check.
            Stmt::Enum(_) => Ok(()),
            Stmt::LetTuple {
                names, value, span, ..
            } => {
                self.check_expr(value)?;
                for n in names {
                    self.declare(
                        n,
                        span,
                        VarInfo {
                            is_const: false,
                            class: None,
                            ty: None,
                            is_import: false,
                            ..Default::default()
                        },
                    )?;
                }
                Ok(())
            }
            Stmt::Import { path, alias, span } => {
                let (names, unexported) = crate::modules::module_info_of_in(
                    path,
                    &self.source_file,
                    span,
                    &self.module_dirs,
                )?;
                for (u_name, mod_display, kind) in unexported {
                    self.unexported_module_names
                        .insert(u_name, (mod_display, kind));
                }
                if let Some(alias_name) = alias {
                    let local_here = self
                        .scopes
                        .last()
                        .and_then(|s| s.get(alias_name))
                        .map(|v| !v.is_import)
                        .unwrap_or(false);
                    if local_here && !self.predeclared.contains(alias_name) {
                        return Err(self.err(
                            span,
                            format!("import '{}' shadows a local binding", alias_name),
                        ));
                    }
                    if let Some(v) = self.scopes.last().and_then(|s| s.get(alias_name)) {
                        if v.is_import {
                            return Err(self.err(
                                span,
                                format!(
                                    "import collision: '{}' is imported multiple times",
                                    alias_name
                                ),
                            ));
                        }
                    }
                    // The alias is a namespace: remember the module's
                    // `public const` exports so a rebind through it can be
                    // rejected (`mu["VERSION"] = 1`).
                    let consts: Vec<String> = names
                        .iter()
                        .filter(|(_, is_const)| *is_const)
                        .map(|(n, _)| n.clone())
                        .collect();
                    let scope = self.scopes.last_mut().unwrap();
                    scope.insert(
                        alias_name.clone(),
                        VarInfo {
                            is_const: true,
                            class: None,
                            ty: None,
                            is_import: true,
                            kind: BindKind::Import,
                            decl: Some(span.clone()),
                            import_consts: if consts.is_empty() {
                                None
                            } else {
                                Some(consts)
                            },
                            ..Default::default()
                        },
                    );
                } else {
                    for (n, is_const) in names {
                        // Collision detection against a same-named LOCAL binding
                        // in this scope.
                        let local_here = self
                            .scopes
                            .last()
                            .and_then(|s| s.get(&n))
                            .map(|v| !v.is_import)
                            .unwrap_or(false);
                        if local_here && !self.predeclared.contains(&n) {
                            return Err(
                                self.err(span, format!("import '{}' shadows a local binding", n))
                            );
                        }
                        // Collision detection against an existing IMPORT binding in this scope.
                        if let Some(v) = self.scopes.last().and_then(|s| s.get(&n)) {
                            if v.is_import {
                                return Err(self.err(
                                    span,
                                    format!(
                                        "import collision: '{}' is imported by multiple modules",
                                        n
                                    ),
                                ));
                            }
                        }
                        let scope = self.scopes.last_mut().unwrap();
                        scope.insert(
                            n,
                            VarInfo {
                                is_const,
                                class: None,
                                ty: None,
                                is_import: true,
                                kind: BindKind::Import,
                                decl: Some(span.clone()),
                                ..Default::default()
                            },
                        );
                    }
                }
                Ok(())
            }
            Stmt::Fn(f) => {
                // Erased generics: make the parameter names visible to the
                // return-type annotation and the body (so `-> T` resolves).
                self.type_params.push(f.type_params.clone());
                if f.params.iter().any(|p| p.name == "self") {
                    return Err(self.err(&f.span, "self is only allowed in methods"));
                }
                if let Some(name) = &f.name {
                    // A local fn may not shadow an imported binding in the
                    // SAME scope: the file-mode case is an import that
                    // overwrote this chunk's pass-1 pre-declaration (probe
                    // p09); the REPL case is caught at pass 1. Ordinary
                    // nested-scope shadowing stays legal, exactly like `let`.
                    if let Some(v) = self.scopes.last().and_then(|s| s.get(name)) {
                        if v.is_import {
                            return Err(self.err(
                                &f.span,
                                format!("local '{}' shadows an imported binding", name),
                            ));
                        }
                    }
                }
                if self.scopes.len() > 1 {
                    if let Some(name) = &f.name {
                        self.declare(
                            name,
                            &f.span,
                            VarInfo {
                                is_const: false,
                                class: None,
                                // F5: nested functions carry their signature.
                                ty: Some(self.fn_sig_type(f)),
                                is_import: false,
                                ..Default::default()
                            },
                        )?;
                    }
                }
                self.fn_depth += 1;
                let was_main_fn = self.in_main_fn;
                if self.is_main && f.name.as_deref() == Some("main") {
                    self.in_main_fn = true;
                }
                let was_async = self.in_async;
                self.in_async = f.is_async;
                // `break`/`continue` are lexically scoped to the loops of the
                // function they appear in: entering a function body starts a
                // fresh loop context (the tree engine raises "break outside a
                // loop" at runtime for this shape; without the reset the
                // checker accepts it and the VM compiler then panics on a
                // `break` bound for an enclosing loop it cannot see).
                let was_loop_depth = std::mem::replace(&mut self.loop_depth, 0);
                self.push_scope();
                let mut names = HashSet::new();
                if f.params.len() > 255 {
                    return Err(self.err(&f.span, "too many parameters in one function (max 255)"));
                }
                for p in &f.params {
                    if !names.insert(p.name.clone()) {
                        return Err(self.err(&p.span, format!("duplicate parameter '{}'", p.name)));
                    }
                    if let Some(d) = &p.default {
                        if !Self::is_const_default(d) {
                            return Err(self.err(
                                &p.span,
                                format!(
                                    "the default for parameter '{}' must be a constant \
                                     (a literal number, string, bool, or null)",
                                    p.name
                                ),
                            ));
                        }
                    }
                    let class = p.ty.as_ref().and_then(|t| {
                        if self.classes.contains_key(&t.name) {
                            Some(t.name.clone())
                        } else {
                            None
                        }
                    });
                    self.declare(
                        &p.name,
                        &p.span,
                        VarInfo {
                            is_const: false,
                            class,
                            ty: p.ty.as_ref().map(|t| self.resolve_type(t)),
                            is_import: false,
                            kind: BindKind::Param,
                            ..Default::default()
                        },
                    )?;
                }
                if let Some(ret) = &f.ret {
                    self.check_type_name(&ret.name, &ret.span)?;
                }
                let saved_ret = self.current_ret.take();
                self.current_ret = f.ret.as_ref().map(|t| self.resolve_type(t));
                let r = self.check_stmts(&f.body.stmts);
                self.in_main_fn = was_main_fn;
                self.current_ret = saved_ret;
                self.pop_scope();
                self.in_async = was_async;
                self.loop_depth = was_loop_depth;
                self.fn_depth -= 1;
                self.type_params.pop();
                r
            }
            Stmt::Class(c) => {
                self.current_class = Some(c.name.clone());
                self.type_params.push(c.type_params.clone());
                // method bodies: self + fields in scope
                for m in &c.methods {
                    if m.params.len() > 255 {
                        return Err(
                            self.err(&m.span, "too many parameters in one function (max 255)")
                        );
                    }
                    // `abstract fn m(params);` is a *declaration*: the body is
                    // empty, so binding `self` and the parameters would only
                    // produce an "unused variable 'self'" warning for a method
                    // that has no code to use anything. Check the signature
                    // and move on - a concrete subclass supplies the body.
                    let is_decl = m
                        .name
                        .as_deref()
                        .is_some_and(|n| c.abstracts.iter().any(|a| a == n));
                    if is_decl {
                        for p in &m.params {
                            if let Some(t) = &p.ty {
                                self.check_type_name(&t.name, &t.span)?;
                            }
                        }
                        if let Some(ret) = &m.ret {
                            self.check_type_name(&ret.name, &ret.span)?;
                        }
                        continue;
                    }
                    self.fn_depth += 1;
                    let was_async = self.in_async;
                    self.in_async = m.is_async;
                    // Fresh loop context per method body (see Stmt::Fn).
                    let was_loop_depth = std::mem::replace(&mut self.loop_depth, 0);
                    self.push_scope();
                    self.type_params.push(m.type_params.clone());
                    let is_ctor = m.name.as_deref() == Some("new");
                    if is_ctor {
                        // constructor parameters do not include self, but the
                        // constructor body still sees the instance
                        self.declare(
                            "self",
                            &m.span,
                            VarInfo {
                                is_const: false,
                                class: Some(c.name.clone()),
                                ty: Some(Ty::Class(c.name.clone())),
                                is_import: false,
                                ..Default::default()
                            },
                        )?;
                        let mut names = HashSet::new();
                        for p in &m.params {
                            if !names.insert(p.name.clone()) {
                                return Err(
                                    self.err(&p.span, format!("duplicate parameter '{}'", p.name))
                                );
                            }
                            self.declare(
                                &p.name,
                                &p.span,
                                VarInfo {
                                    is_const: false,
                                    class: None,
                                    ty: p.ty.as_ref().map(|t| self.resolve_type(t)),
                                    is_import: false,
                                    ..Default::default()
                                },
                            )?;
                        }
                    } else {
                        self.declare(
                            "self",
                            &m.span,
                            VarInfo {
                                is_const: false,
                                class: Some(c.name.clone()),
                                ty: Some(Ty::Class(c.name.clone())),
                                is_import: false,
                                ..Default::default()
                            },
                        )?;
                        let mut names = HashSet::new();
                        for p in m.params.iter().skip(1) {
                            if !names.insert(p.name.clone()) {
                                return Err(
                                    self.err(&p.span, format!("duplicate parameter '{}'", p.name))
                                );
                            }
                            self.declare(
                                &p.name,
                                &p.span,
                                VarInfo {
                                    is_const: false,
                                    class: None,
                                    ty: p.ty.as_ref().map(|t| self.resolve_type(t)),
                                    is_import: false,
                                    ..Default::default()
                                },
                            )?;
                        }
                    }
                    let saved_ret = self.current_ret.take();
                    self.current_ret = m.ret.as_ref().map(|t| self.resolve_type(t));
                    let r = self.check_stmts(&m.body.stmts);
                    self.current_ret = saved_ret;
                    self.pop_scope();
                    self.type_params.pop();
                    self.in_async = was_async;
                    self.loop_depth = was_loop_depth;
                    self.fn_depth -= 1;
                    r?;
                }
                self.current_class = None;
                self.type_params.pop();
                Ok(())
            }
            Stmt::Interface(_) => Ok(()),
            Stmt::Let {
                name,
                ty,
                value,
                span,
                ..
            } => {
                if let Some(t) = ty {
                    self.check_type_name(&t.name, &t.span)?;
                }
                let class = match value {
                    Some(Expr::New { class, .. }) if self.classes.contains_key(class) => {
                        Some(class.clone())
                    }
                    _ => ty.as_ref().and_then(|t| {
                        if self.classes.contains_key(&t.name) {
                            Some(t.name.clone())
                        } else {
                            None
                        }
                    }),
                };
                if let Some(e) = value {
                    self.check_expr(e)?;
                }
                if let (Some(t), Some(v)) = (ty, value) {
                    let want = self.resolve_type(t);
                    let got = self.type_of(v);
                    if !self.assignable(&got, &want) {
                        return Err(self.mismatch(
                            v.span(),
                            format!(
                                "type mismatch in '{}': expected {}, got {}",
                                name,
                                want.name(),
                                got.name()
                            ),
                        ));
                    }
                }
                // F12: a local binding may not shadow an imported binding in
                // the same scope (the import-then-local direction; the local-
                // then-import direction is diagnosed at the import itself).
                if let Some(v) = self.scopes.last().and_then(|s| s.get(name)) {
                    if v.is_import {
                        return Err(self.err(
                            span,
                            format!("local '{}' shadows an imported binding", name),
                        ));
                    }
                }
                self.declare(
                    name,
                    span,
                    VarInfo {
                        is_const: false,
                        class,
                        // F5: an unannotated let keeps the value's function
                        // SIGNATURE when it binds a function value, so calls
                        // through the new name are checked; everything else
                        // stays dynamically typed (Any).
                        ty: ty
                            .as_ref()
                            .map(|t| self.resolve_type(t))
                            .or_else(|| value.as_ref().and_then(|v| self.fn_value_type(v))),
                        is_import: false,
                        ..Default::default()
                    },
                )
            }
            Stmt::Const {
                name,
                ty,
                value,
                span,
                ..
            } => {
                if let Some(t) = ty {
                    self.check_type_name(&t.name, &t.span)?;
                }
                self.check_expr(value)?;
                if let Some(t) = ty {
                    let want = self.resolve_type(t);
                    let got = self.type_of(value);
                    if !self.assignable(&got, &want) {
                        return Err(self.mismatch(
                            value.span(),
                            format!(
                                "type mismatch in '{}': expected {}, got {}",
                                name,
                                want.name(),
                                got.name()
                            ),
                        ));
                    }
                }
                // F12: same-scope import collision, as in Stmt::Let.
                if let Some(v) = self.scopes.last().and_then(|s| s.get(name)) {
                    if v.is_import {
                        return Err(self.err(
                            span,
                            format!("local '{}' shadows an imported binding", name),
                        ));
                    }
                }
                self.declare(
                    name,
                    span,
                    VarInfo {
                        is_const: true,
                        class: None,
                        // F5: propagate a function signature, as in Stmt::Let.
                        ty: ty
                            .as_ref()
                            .map(|t| self.resolve_type(t))
                            .or_else(|| self.fn_value_type(value)),
                        is_import: false,
                        ..Default::default()
                    },
                )
            }
            Stmt::If {
                cond,
                then,
                elifs,
                els,
                ..
            } => {
                self.check_expr(cond)?;
                self.check_block(then)?;
                for (c, b) in elifs {
                    self.check_expr(c)?;
                    self.check_block(b)?;
                }
                if let Some(b) = els {
                    self.check_block(b)?;
                }
                Ok(())
            }
            Stmt::While { cond, body, .. } => {
                self.check_expr(cond)?;
                self.loop_depth += 1;
                let r = self.check_block(body);
                self.loop_depth -= 1;
                r
            }
            Stmt::ForIn {
                var,
                var2,
                iter,
                body,
                ..
            } => {
                self.check_expr(iter)?;
                self.push_scope();
                self.declare(
                    var,
                    s.span(),
                    VarInfo {
                        is_const: false,
                        class: None,
                        ty: None,
                        is_import: false,
                        ..Default::default()
                    },
                )?;
                // `for (k, v) in m` also binds the value.
                if let Some(v2) = var2 {
                    self.declare(
                        v2,
                        s.span(),
                        VarInfo {
                            is_const: false,
                            class: None,
                            ty: None,
                            is_import: false,
                            ..Default::default()
                        },
                    )?;
                }
                self.loop_depth += 1;
                let r = self.check_stmts(&body.stmts);
                self.loop_depth -= 1;
                self.pop_scope();
                r
            }
            Stmt::ForC {
                init,
                cond,
                post,
                body,
                ..
            } => {
                self.push_scope();
                if let Stmt::Let {
                    name,
                    ty,
                    value,
                    span,
                    ..
                } = &**init
                {
                    if let Some(t) = ty {
                        self.check_type_name(&t.name, &t.span)?;
                    }
                    if let Some(e) = value {
                        self.check_expr(e)?;
                    }
                    if let (Some(t), Some(v)) = (ty, value) {
                        let want = self.resolve_type(t);
                        let got = self.type_of(v);
                        if !self.assignable(&got, &want) {
                            return Err(self.mismatch(
                                v.span(),
                                format!(
                                    "type mismatch in '{}': expected {}, got {}",
                                    name,
                                    want.name(),
                                    got.name()
                                ),
                            ));
                        }
                    }
                    let tyo = ty.as_ref().map(|t| self.resolve_type(t));
                    self.declare(
                        name,
                        span,
                        VarInfo {
                            is_const: false,
                            class: None,
                            ty: tyo,
                            is_import: false,
                            ..Default::default()
                        },
                    )?;
                }
                self.check_expr(cond)?;
                self.check_expr(post)?;
                self.loop_depth += 1;
                let r = self.check_stmts(&body.stmts);
                self.loop_depth -= 1;
                self.pop_scope();
                r
            }
            Stmt::Return { value, span } => {
                if self.fn_depth == 0 {
                    return Err(self.err(span, "return outside a function"));
                }
                if let Some(e) = value {
                    self.check_expr(e)?;
                }
                if let Some(want) = &self.current_ret {
                    if let Some(e) = value {
                        let got = self.type_of(e);
                        if !self.assignable(&got, want) {
                            return Err(self.mismatch(
                                span,
                                format!(
                                    "return type mismatch: expected {}, got {}",
                                    want.name(),
                                    got.name()
                                ),
                            ));
                        }
                    } else {
                        return Err(self.err(
                            span,
                            format!(
                                "function declared to return {} but returns nothing",
                                want.name()
                            ),
                        ));
                    }
                }
                Ok(())
            }
            Stmt::Break { span } => {
                if self.loop_depth == 0 {
                    return Err(self.err(span, "break outside a loop"));
                }
                Ok(())
            }
            Stmt::Continue { span } => {
                if self.loop_depth == 0 {
                    return Err(self.err(span, "continue outside a loop"));
                }
                Ok(())
            }
            Stmt::Block(b) => self.check_block(b),
            Stmt::Expr(e) => self.check_expr(e),
            Stmt::Spawn { callable, .. } => {
                // Any expression that evaluates to a function: the runtime
                // reports a clear error when the value is not callable, and a
                // static check would reject `spawn obj.handler` and
                // `spawn some_closure` for no good reason.
                self.check_expr(callable)
            }
            Stmt::Try {
                body,
                catch,
                finally,
                ..
            } => {
                self.check_block(body)?;
                if let Some(cc) = catch {
                    self.push_scope();
                    self.declare(
                        &cc.name,
                        &cc.span,
                        VarInfo {
                            is_const: false,
                            class: None,
                            ty: None,
                            is_import: false,
                            ..Default::default()
                        },
                    )?;
                    let r = self.check_stmts(&cc.body.stmts);
                    self.pop_scope();
                    r?;
                }
                if let Some(fb) = finally {
                    self.check_block(fb)?;
                }
                Ok(())
            }
            Stmt::Throw { value, .. } => self.check_expr(value),
        }
    }

    /// Resolve a parsed `: Type` annotation into a `Ty`. Unknown-but-validated
    /// names fall back to `Any`; `check_type_name` has already rejected truly
    /// unknown names before this is called.
    fn resolve_type(&self, t: &TypeExpr) -> Ty {
        let lower = t.name.to_ascii_lowercase();
        match lower.as_str() {
            "any" => Ty::Any,
            "int" => Ty::Int,
            "float" => Ty::Float,
            "bool" => Ty::Bool,
            "string" => Ty::Str,
            "null" => Ty::Null,
            "list" | "vec" | "stack" | "queue" | "deque" | "linked_list" | "singly_linked_list"
            | "doubly_linked_list" | "map" | "set" | "tuple" | "binary_tree" | "bst" | "heap"
            | "priority_queue" | "graph" | "hash_table" | "trie" | "matrix" | "vector"
            | "bytes" | "ring_buffer" | "bag" | "ref" | "option" | "result" | "task"
            | "channel" | "mutex" | "some" | "none" | "ok" | "err" => {
                let elem = t.arg.as_ref().map(|a| Box::new(self.resolve_type(a)));
                Ty::Coll(lower, elem)
            }
            _ => {
                if self.classes.contains_key(&t.name) {
                    Ty::Class(t.name.clone())
                } else if self.interfaces.contains_key(&t.name) {
                    Ty::Interface(t.name.clone())
                } else {
                    Ty::Any
                }
            }
        }
    }

    /// Best-effort static type of an expression. Returns `Any` whenever the type
    /// cannot be determined (which keeps unannotated code fully dynamic).
    /// Best-effort static class of a receiver expression (F6): `self` maps to
    /// the class whose member is being checked; a binding contributes its
    /// declared class or class annotation; any other expression falls back to
    /// the gradual type computer. Only locally declared classes are returned —
    /// imported or builtin-class receivers stay dynamically unchecked, exactly
    /// as bare-Ident receivers behaved before.
    fn receiver_class(&self, e: &Expr) -> Option<String> {
        match e {
            Expr::Ident(n, _) if n == "self" => self.current_class.clone(),
            Expr::Ident(n, _) => {
                let v = self.lookup(n)?;
                v.class.clone().or_else(|| match &v.ty {
                    Some(Ty::Class(c)) if self.classes.contains_key(c) => Some(c.clone()),
                    _ => None,
                })
            }
            other => match self.type_of(other) {
                Ty::Class(c) if self.classes.contains_key(&c) => Some(c),
                _ => None,
            },
        }
    }

    /// Static type of a function value. Carries the parameter/return
    /// signature so call checking works through let-bound functions, nested
    /// declarations, and lambdas — not only direct calls (F5).
    fn fn_sig_type(&self, f: &FnProto) -> Ty {
        let params = f
            .params
            .iter()
            .map(|p| p.ty.as_ref().map(|t| self.resolve_type(t)))
            .collect();
        Ty::Fn(Some(std::sync::Arc::new(ty::FnSig {
            arity: f.params.len(),
            min_arity: f.min_arity(),
            params,
            ret: f.ret.as_ref().map(|t| self.resolve_type(t)),
        })))
    }

    /// F5 helper: the propagated type of a let-bound function value, or
    /// `None` for anything that is not a (annotated or not) function.
    fn fn_value_type(&self, e: &Expr) -> Option<Ty> {
        match e {
            Expr::Fn(proto) => Some(self.fn_sig_type(proto)),
            Expr::Ident(n, _) => self.lookup(n).and_then(|v| match &v.ty {
                Some(t @ Ty::Fn(_)) => Some(t.clone()),
                _ => None,
            }),
            _ => None,
        }
    }

    /// F5 helper: validate a call against a propagated signature (arity plus
    /// annotated parameter types), with the same wording as the direct-call
    /// path so diagnostics stay consistent.
    fn check_sig_args(
        &self,
        label: &str,
        sig: &ty::FnSig,
        args: &[Expr],
        span: &Span,
    ) -> SpidResult<()> {
        if Self::has_named_args(args) {
            // FnSig carries no parameter names, so a call through a function
            // VALUE cannot resolve named arguments — reject rather than
            // silently mis-bind.
            return Err(self.err(
                span,
                format!(
                    "named arguments on a call through '{}' are not supported yet \
                     (call the declared function directly)",
                    label
                ),
            ));
        }
        if args.len() < sig.min_arity || args.len() > sig.arity {
            let want = if sig.min_arity == sig.arity {
                format!("{} argument(s)", sig.arity)
            } else {
                format!("{} to {} argument(s)", sig.min_arity, sig.arity)
            };
            return Err(self.err(
                span,
                format!("{} expects {}, got {}", label, want, args.len()),
            ));
        }
        for (i, a) in args.iter().enumerate() {
            if let Some(Some(want)) = sig.params.get(i) {
                let got = self.type_of(a);
                if !self.assignable(&got, want) {
                    return Err(self.err(
                        a.span(),
                        format!(
                            "argument {} of '{}' expects {}, got {}",
                            i + 1,
                            label,
                            want.name(),
                            got.name()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// The static type of a value-position `if`: the two branches must agree,
    /// and a missing `else` contributes `null`.
    fn branch_type(&self, then: &Block, els: Option<&Block>) -> Ty {
        let a = block_value_type(self, then);
        let b = match els {
            Some(b) => block_value_type(self, b),
            None => Ty::Null,
        };
        common_type(a, b)
    }

    fn type_of(&self, e: &Expr) -> Ty {
        match e {
            // A named argument is only legal inside a call's argument list,
            // where the caller checks it against the signature; it has no
            // standalone type.
            Expr::NamedArg { .. } => Ty::Any,
            Expr::Int(_, _) => Ty::Int,
            Expr::Float(_, _) => Ty::Float,
            Expr::Str(_, _) => Ty::Str,
            Expr::Bool(_, _) => Ty::Bool,
            Expr::Null(_) => Ty::Null,
            Expr::If { then, els, .. } => self.branch_type(then, els.as_ref()),
            Expr::Match { arms, .. } => arms
                .iter()
                .map(|a| block_value_type(self, &a.body))
                .fold(Ty::Null, common_type),
            // A null-safe field read can always produce null, so the only honest
            // static type is `any`.
            Expr::OptField { .. } => Ty::Any,
            // The payload type of an option/result is not tracked, so `?` is
            // dynamically typed.
            Expr::Propagate(..) => Ty::Any,
            Expr::Coalesce { l, r, .. } => common_type(self.type_of(l), self.type_of(r)),
            Expr::Ident(name, _) => {
                if name == "self" {
                    self.current_class.clone().map(Ty::Class).unwrap_or(Ty::Any)
                } else {
                    self.lookup(name)
                        .and_then(|v| v.ty.clone())
                        .unwrap_or(Ty::Any)
                }
            }
            Expr::List(items, _) => {
                let mut elem: Option<Ty> = None;
                for it in items {
                    let t = self.type_of(it);
                    match (&elem, &t) {
                        (None, _) => elem = Some(t),
                        (Some(a), b) if a == b => {}
                        _ => {
                            elem = Some(Ty::Any);
                            break;
                        }
                    }
                }
                Ty::Coll("list".to_string(), elem.map(Box::new))
            }
            Expr::Fn(f) => self.fn_sig_type(f),
            Expr::Unary { op, expr, .. } => match op {
                UnOp::Neg => {
                    let t = self.type_of(expr);
                    if t == Ty::Int || t == Ty::Float {
                        t
                    } else {
                        Ty::Any
                    }
                }
                UnOp::Not => Ty::Bool,
            },
            Expr::Binary { op, l, r, .. } => match op {
                BinOp::Add => {
                    let a = self.type_of(l);
                    let b = self.type_of(r);
                    if a == Ty::Str && b == Ty::Str {
                        return Ty::Str;
                    }
                    if let (Ty::Coll(n1, _), Ty::Coll(n2, _)) = (&a, &b) {
                        if n1 == "list" && n2 == "list" {
                            return Ty::Coll("list".to_string(), None);
                        }
                    }
                    if a == Ty::Int && b == Ty::Int {
                        return Ty::Int;
                    }
                    if matches!(a, Ty::Float) || matches!(b, Ty::Float) {
                        return Ty::Float;
                    }
                    Ty::Any
                }
                BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    let a = self.type_of(l);
                    let b = self.type_of(r);
                    if a == Ty::Int && b == Ty::Int {
                        Ty::Int
                    } else if a == Ty::Float || b == Ty::Float {
                        Ty::Float
                    } else {
                        Ty::Any
                    }
                }
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Ty::Bool,
            },
            Expr::Logical { .. } => Ty::Bool,
            Expr::Super { .. } | Expr::SuperCall { .. } => Ty::Any,
            Expr::Call { callee, args, .. } => {
                if let Expr::Ident(name, _) = &**callee {
                    if let Some(fi) = self.funcs.get(name) {
                        return fi
                            .ret
                            .as_ref()
                            .map(|t| self.resolve_type(t))
                            .unwrap_or(Ty::Any);
                    }
                    if BUILTIN_CONSTRUCTORS.contains(&name.as_str()) {
                        let mut elem: Option<Ty> = None;
                        for a in args {
                            let t = self.type_of(a);
                            match (&elem, &t) {
                                (None, _) => elem = Some(t),
                                (Some(x), y) if x == y => {}
                                _ => {
                                    elem = Some(Ty::Any);
                                    break;
                                }
                            }
                        }
                        return Ty::Coll(coll_kind(name).to_string(), elem.map(Box::new));
                    }
                    if let Some(rt) = builtin_return_type(name) {
                        return rt;
                    }
                    // F5: a signature-carrying function value bound to a name.
                    if let Ty::Fn(Some(sig)) = self
                        .lookup(name)
                        .and_then(|v| v.ty.clone())
                        .unwrap_or(Ty::Any)
                    {
                        return sig.ret.clone().unwrap_or(Ty::Any);
                    }
                } else if let Ty::Fn(Some(sig)) = self.type_of(callee) {
                    return sig.ret.clone().unwrap_or(Ty::Any);
                }
                Ty::Any
            }
            Expr::Method { object, name, .. } => {
                let recv = self.type_of(object);
                if let Ty::Class(c) = recv {
                    if let Some(mi) = self.method_sig(&c, name) {
                        return mi
                            .ret
                            .as_ref()
                            .map(|t| self.resolve_type(t))
                            .unwrap_or(Ty::Any);
                    }
                }
                Ty::Any
            }
            Expr::Index { object, .. } => {
                if let Ty::Coll(_, Some(e)) = self.type_of(object) {
                    *e
                } else {
                    Ty::Any
                }
            }
            Expr::Field { object, name, .. } => {
                if let Ty::Class(c) = self.type_of(object) {
                    if let Some(ci) = self.classes.get(&c) {
                        if let Some((_, Some(ft))) = ci.fields.get(name) {
                            return self.resolve_type(ft);
                        }
                    }
                }
                Ty::Any
            }
            Expr::New { class, .. } => {
                if self.classes.contains_key(class) {
                    Ty::Class(class.clone())
                } else {
                    Ty::Any
                }
            }
            Expr::Assign { value, .. } => self.type_of(value),
            Expr::Await(inner, _) => self.type_of(inner),
            Expr::Spawn { .. } => Ty::Any,
        }
    }

    /// Gradual assignability: is a value of static type `from` acceptable where
    /// `to` is expected? `Any` is the top type (accepts/is accepted by
    /// anything); `Int` widens to `Float`; classes accept subclasses and the
    /// interfaces they implement.
    fn assignable(&self, from: &Ty, to: &Ty) -> bool {
        if let Ty::Any = to {
            return true;
        }
        if let Ty::Any = from {
            return true;
        }
        match (from, to) {
            (Ty::Int, Ty::Int)
            | (Ty::Float, Ty::Float)
            | (Ty::Bool, Ty::Bool)
            | (Ty::Str, Ty::Str)
            | (Ty::Null, Ty::Null) => true,
            (Ty::Int, Ty::Float) => true, // numeric widening
            (Ty::Null, Ty::Class(_))
            | (Ty::Null, Ty::Interface(_))
            | (Ty::Null, Ty::Coll(_, _)) => true,
            (Ty::Class(c), Ty::Class(p)) => c == p || self.is_subclass(c, p),
            (Ty::Class(c), Ty::Interface(i)) => self.implements(c, i),
            (Ty::Coll(a, ea), Ty::Coll(b, eb)) => {
                if a != b {
                    return false;
                }
                if a == "result" {
                    // `result<int>` cannot name the error side, so the element
                    // types are not comparable.
                    return true;
                }
                match (ea, eb) {
                    // An unknown element type (an empty literal, `none()`,
                    // `map()`) is compatible with anything.
                    (Some(x), Some(y)) => self.assignable(x, y) || self.assignable(y, x),
                    _ => true,
                }
            }
            // A10: function types are compared by signature, not treated as
            // interchangeable. `Ty::Fn(None)` is an unannotated function (fully
            // dynamic under gradual typing) and stays permissive; when both
            // sides carry a signature, arity must match, annotated parameters
            // must agree, and the return type must be assignable.
            (Ty::Fn(a), Ty::Fn(b)) => match (a.as_deref(), b.as_deref()) {
                (Some(from), Some(to)) => self.fn_sig_assignable(from, to),
                _ => true,
            },
            _ => false,
        }
    }

    /// Is a function with signature `from` usable where `to` is expected?
    /// Parameters are compared invariantly (a caller may pass either type), and
    /// only where both sides are annotated; the return type follows ordinary
    /// assignability (widening allowed).
    fn fn_sig_assignable(&self, from: &ty::FnSig, to: &ty::FnSig) -> bool {
        if from.arity != to.arity {
            return false;
        }
        for (a, b) in from.params.iter().zip(to.params.iter()) {
            if let (Some(x), Some(y)) = (a, b) {
                if !self.assignable(x, y) && !self.assignable(y, x) {
                    return false;
                }
            }
        }
        match (&from.ret, &to.ret) {
            (Some(x), Some(y)) => self.assignable(x, y),
            _ => true,
        }
    }

    fn is_subclass(&self, child: &str, ancestor: &str) -> bool {
        let mut cur = Some(child.to_string());
        let mut seen = HashSet::new();
        while let Some(c) = cur {
            if c == ancestor {
                return true;
            }
            if !seen.insert(c.clone()) {
                return false;
            }
            cur = self.classes.get(&c).and_then(|ci| ci.parent.clone());
        }
        false
    }

    fn implements(&self, class: &str, iface: &str) -> bool {
        let mut cur = Some(class.to_string());
        let mut seen = HashSet::new();
        while let Some(c) = cur {
            if !seen.insert(c.clone()) {
                return false;
            }
            if let Some(ci) = self.classes.get(&c) {
                if ci.interfaces.iter().any(|i| i == iface) {
                    return true;
                }
                cur = ci.parent.clone();
            } else {
                return false;
            }
        }
        false
    }

    fn check_type_name(&self, name: &str, span: &Span) -> SpidResult<()> {
        const TYPES: &[&str] = &[
            "int",
            "float",
            "bool",
            "string",
            "list",
            "vec",
            "stack",
            "queue",
            "deque",
            "linked_list",
            "singly_linked_list",
            "doubly_linked_list",
            "map",
            "set",
            "tuple",
            "binary_tree",
            "bst",
            "heap",
            "priority_queue",
            "graph",
            "hash_table",
            "trie",
            "matrix",
            "vector",
            "bytes",
            "ring_buffer",
            "bag",
            "ref",
            "option",
            "result",
            "task",
            "channel",
            "mutex",
            "any",
        ];
        if TYPES.iter().any(|t| t.eq_ignore_ascii_case(name))
            || self.classes.contains_key(name)
            || self.interfaces.contains_key(name)
            // An erased generic parameter is in scope for its own declaration.
            || self.type_params.iter().any(|level| level.iter().any(|p| p == name))
        {
            Ok(())
        } else {
            Err(self.err(span, format!("unknown type '{}'", name)))
        }
    }

    fn check_expr(&mut self, e: &Expr) -> SpidResult<()> {
        match e {
            // Named arguments are validated where the call is checked (the
            // callee's signature is known there); a stray one outside a call
            // is a compile error.
            Expr::NamedArg { span, .. } => Err(self.err(
                span,
                "a named argument (name: value) is only valid inside a call",
            )),
            Expr::If {
                cond,
                then,
                els,
                span,
            } => {
                self.check_expr(cond)?;
                for b in std::iter::once(then).chain(els.iter()) {
                    if !block_has_value(b) {
                        return Err(self.err(
                            span,
                            "an if-expression branch must end with an expression (or with \
                             return/throw)",
                        ));
                    }
                }
                self.check_block(then)?;
                if let Some(b) = els {
                    self.check_block(b)?;
                }
                Ok(())
            }
            Expr::OptField { object, .. } => self.check_expr(object),
            Expr::Propagate(inner, _) => self.check_expr(inner),
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => {
                self.check_expr(scrutinee)?;
                if arms.is_empty() {
                    return Err(self.err(span, "a match needs at least one arm"));
                }
                for arm in arms {
                    self.push_scope();
                    self.declare_pattern(&arm.pattern)?;
                    let r = (|| {
                        if let Some(g) = &arm.guard {
                            self.check_expr(g)?;
                        }
                        if !block_has_value(&arm.body) {
                            return Err(self.err(
                                &arm.span,
                                "a match arm must end with an expression (or with \
                                 return/throw)",
                            ));
                        }
                        self.check_block(&arm.body)
                    })();
                    self.pop_scope();
                    r?;
                }
                self.check_exhaustive(arms, span)
            }
            Expr::Coalesce { l, r, .. } => {
                self.check_expr(l)?;
                self.check_expr(r)?;
                Ok(())
            }
            Expr::Int(_, _)
            | Expr::Float(_, _)
            | Expr::Str(_, _)
            | Expr::Bool(_, _)
            | Expr::Null(_) => Ok(()),
            Expr::Ident(name, span) => {
                if name == "self" && self.current_class.is_none() {
                    return Err(self.err(span, "self outside a method"));
                }
                if self.known(name) {
                    self.mark_used(name);
                    Ok(())
                } else {
                    Err(self.unknown_name(span, name))
                }
            }
            Expr::List(items, _) => {
                for it in items {
                    self.check_expr(it)?;
                }
                Ok(())
            }
            Expr::Fn(f) => {
                self.fn_depth += 1;
                let was_async = self.in_async;
                self.in_async = f.is_async;
                // Fresh loop context: a `break` inside a function literal is
                // never bound to a loop outside the literal (see the Stmt::Fn
                // path for the full rationale).
                let was_loop_depth = std::mem::replace(&mut self.loop_depth, 0);
                self.push_scope();
                let mut names = HashSet::new();
                if f.params.len() > 255 {
                    return Err(self.err(&f.span, "too many parameters in one function (max 255)"));
                }
                for p in &f.params {
                    if !names.insert(p.name.clone()) {
                        return Err(self.err(&p.span, format!("duplicate parameter '{}'", p.name)));
                    }
                    if let Some(d) = &p.default {
                        if !Self::is_const_default(d) {
                            return Err(self.err(
                                &p.span,
                                format!(
                                    "the default for parameter '{}' must be a constant \
                                     (a literal number, string, bool, or null)",
                                    p.name
                                ),
                            ));
                        }
                    }
                    self.declare(
                        &p.name,
                        &p.span,
                        VarInfo {
                            is_const: false,
                            class: None,
                            ty: p.ty.as_ref().map(|t| self.resolve_type(t)),
                            is_import: false,
                            kind: BindKind::Param,
                            // B3: inside a `gui_on` handler the (event,
                            // payload) signature is imposed by the runtime;
                            // a handler that needs neither is not a mistake.
                            used: self.gui_handler_depth > 0,
                            ..Default::default()
                        },
                    )?;
                }
                let saved_ret = self.current_ret.take();
                self.current_ret = f.ret.as_ref().map(|t| self.resolve_type(t));
                let r = self.check_stmts(&f.body.stmts);
                self.current_ret = saved_ret;
                self.pop_scope();
                self.in_async = was_async;
                self.loop_depth = was_loop_depth;
                self.fn_depth -= 1;
                r
            }
            Expr::Unary { expr, .. } => self.check_expr(expr),
            Expr::Binary { l, r, .. } => {
                self.check_expr(l)?;
                self.check_expr(r)
            }
            Expr::Logical { l, r, .. } => {
                self.check_expr(l)?;
                self.check_expr(r)
            }
            Expr::Assign {
                target,
                value,
                span,
                ..
            } => {
                match &**target {
                    Expr::Ident(name, _) => match self.lookup(name) {
                        Some(v) if v.is_const => {
                            return Err(self.err(span, format!("cannot assign to const '{}'", name)))
                        }
                        None if !self.known(name) => return Err(self.unknown_name(span, name)),
                        Some(v) => {
                            if let Some(t) = &v.ty {
                                let got = self.type_of(value);
                                if !self.assignable(&got, t) {
                                    return Err(self.mismatch(
                                        span,
                                        format!(
                                            "type mismatch assigning to '{}': expected {}, got {}",
                                            name,
                                            t.detail_name(),
                                            got.detail_name()
                                        ),
                                    ));
                                }
                            }
                        }
                        _ => {}
                    },
                    // A10: the index expression is evaluated at run time
                    // (`a[ghost()] = 1`), so it must be checked statically too.
                    // This arm used to drop it, letting unknown names, arity
                    // errors and privacy violations through `spid3r check`.
                    Expr::Index { object, index, .. } => {
                        self.check_expr(object)?;
                        self.check_expr(index)?;
                        // A module namespace is a read-only view of the
                        // module: its `public const` exports cannot be
                        // rebound through the alias (`mu["VERSION"] = 1`).
                        if let Some(consts) = self.alias_const_exports(object) {
                            if let Some(key) = Self::string_literal(index) {
                                if consts.iter().any(|c| c == key) {
                                    return Err(self.err(
                                        span,
                                        format!(
                                            "cannot assign to constant '{}' (it is a public const of the module)",
                                            key
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                    Expr::Field { object, name, .. } => {
                        self.check_expr(object)?;
                        // F6: field writes are privacy-checked exactly like
                        // reads, for any statically-typed receiver — this arm
                        // previously had no privacy check at all.
                        if let Some(c) = self.receiver_class(object) {
                            match self.field_decl(&c, name) {
                                Some((vis, decl_c)) => {
                                    if vis == Vis::Private
                                        && self.current_class.as_deref() != Some(&decl_c)
                                    {
                                        return Err(self.err(
                                            span,
                                            format!(
                                                "field '{}' of class '{}' is private",
                                                name, decl_c
                                            ),
                                        ));
                                    }
                                }
                                None => {
                                    return Err(self.err(
                                        span,
                                        format!("class '{}' has no field '{}'", c, name),
                                    ))
                                }
                            }
                        }
                        let recv = self.type_of(object);
                        if let Ty::Class(c) = recv {
                            if let Some((_, Some(ft))) =
                                self.classes.get(&c).and_then(|ci| ci.fields.get(name))
                            {
                                let want = self.resolve_type(ft);
                                let got = self.type_of(value);
                                if !self.assignable(&got, &want) {
                                    return Err(self.mismatch(
                                        span,
                                        format!(
                                            "type mismatch assigning to field '{}': expected {}, got {}",
                                            name,
                                            want.name(),
                                            got.name()
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                    _ => return Err(self.err(target.span(), "invalid assignment target")),
                }
                self.check_expr(value)
            }
            // A bare `super` is not a value: it must be called.
            Expr::Super { span } => Err(self.err(
                span,
                "super must be called: write super(...) or super.method(...)",
            )),
            // An already-resolved `super` call (produced by `crate::lower`).
            Expr::SuperCall {
                parent,
                method,
                args,
                span,
            } => {
                self.check_super_in(parent, span, method.as_deref(), args)?;
                for a in args {
                    self.check_arg(a)?;
                }
                Ok(())
            }
            Expr::Call { callee, args, span } => {
                // `super(args)` — the parent class's constructor.
                if let Expr::Super { .. } = &**callee {
                    self.check_super(span, None, args)?;
                    for a in args {
                        self.check_arg(a)?;
                    }
                    return Ok(());
                }
                if let Expr::Ident(name, _) = &**callee {
                    // A call is a READ of the callee: `f()` uses `f`. Without
                    // this the unused-variable lint fired on every closure held
                    // in a variable and then called.
                    self.mark_used(name);
                    if let Some(fi) = self.funcs.get(name).cloned() {
                        if Self::has_named_args(args) {
                            self.check_named_args(args, &fi.param_names, fi.min_arity, name, span)?;
                        } else if args.len() < fi.min_arity || args.len() > fi.arity {
                            let want = if fi.min_arity == fi.arity {
                                format!("{} argument(s)", fi.arity)
                            } else {
                                format!("{} to {} argument(s)", fi.min_arity, fi.arity)
                            };
                            return Err(self.err(
                                span,
                                format!("{} expects {}, got {}", name, want, args.len()),
                            ));
                        }
                        for (i, a) in args.iter().enumerate() {
                            if let Some(Some(pt)) = fi.params.get(i) {
                                let want = self.resolve_type(pt);
                                let got = self.type_of(a);
                                if !self.assignable(&got, &want) {
                                    return Err(self.err(
                                        a.span(),
                                        format!(
                                            "argument {} of '{}' expects {}, got {}",
                                            i + 1,
                                            name,
                                            want.name(),
                                            got.name()
                                        ),
                                    ));
                                }
                            }
                        }
                    } else {
                        // F5: a signature-carrying function value bound to a
                        // name (`let g = f`, a nested fn, an imported fn
                        // value) is checked through its propagated signature
                        // instead of being skipped as a dynamic callee.
                        let sig = self.lookup(name).and_then(|v| match &v.ty {
                            Some(Ty::Fn(Some(s))) => Some(s.clone()),
                            _ => None,
                        });
                        if let Some(sig) = sig {
                            self.check_sig_args(name, &sig, args, span)?;
                        } else if Self::has_named_args(args) {
                            return Err(self.err(
                                span,
                                format!(
                                    "named arguments on a call to '{}' are not supported: \
                                     the callee is not a statically-known function",
                                    name
                                ),
                            ));
                        } else if !self.known(name) {
                            return Err(self.unknown_name(span, name));
                        }
                    }
                } else {
                    self.check_expr(callee)?;
                    // F5: a non-Ident callee whose static type carries a
                    // signature — e.g. a call returning a function value.
                    if let Ty::Fn(Some(sig)) = self.type_of(callee) {
                        self.check_sig_args("<expression>", &sig, args, span)?;
                    } else if Self::has_named_args(args) {
                        return Err(self.err(
                            span,
                            "named arguments on a call through '<expression>' are not supported yet \
                             (call the declared function directly)",
                        ));
                    }
                }
                for a in args {
                    // B3: `gui_on(event, handler)` requires a two-parameter
                    // handler; do not warn about parameters the handler never
                    // reads (the runtime imposes the signature).
                    if matches!(&**callee, Expr::Ident(n, _) if n == "gui_on") {
                        self.gui_handler_depth += 1;
                        self.check_arg(a)?;
                        self.gui_handler_depth -= 1;
                    } else {
                        self.check_arg(a)?;
                    }
                }
                Ok(())
            }
            Expr::Method {
                object,
                name,
                args,
                span,
            } => {
                // `super.m(args)` — the parent class's implementation of `m`.
                if let Expr::Super { .. } = &**object {
                    self.check_super(span, Some(name), args)?;
                    for a in args {
                        self.check_arg(a)?;
                    }
                    return Ok(());
                }
                self.check_expr(object)?;
                for a in args {
                    self.check_arg(a)?;
                }
                // `mu.set("VERSION", 1)` on a module namespace would rebind a
                // `public const` export — the same rejection the index form
                // gets.
                if name == "set" {
                    if let Some(consts) = self.alias_const_exports(object) {
                        if let Some(key) = args.first().and_then(|a| Self::string_literal(a)) {
                            if consts.iter().any(|c| c == key) {
                                return Err(self.err(
                                    span,
                                    format!(
                                        "cannot assign to constant '{}' (it is a public const of the module)",
                                        key
                                    ),
                                ));
                            }
                        }
                    }
                }
                if Self::has_named_args(args) {
                    return Err(self.err(
                        span,
                        format!(
                            "named arguments on method calls are not supported yet \
                             (call {} as a function, or pass arguments positionally)",
                            name
                        ),
                    ));
                }
                // When the receiver class is statically known, check it — for
                // ANY receiver expression (F6), not only bare Idents.
                let recv_class = self.receiver_class(object);
                if let Some(cls) = recv_class {
                    match self.method_decl(&cls, name) {
                        Some((m, decl_cls)) => {
                            if m.vis == Vis::Private
                                && self.current_class.as_deref() != Some(&decl_cls)
                            {
                                return Err(self.err(
                                    span,
                                    format!("method '{}' of class '{}' is private", name, decl_cls),
                                ));
                            }
                            let (vis, arity, arity_min) = (m.vis, m.arity(), m.min_arity);
                            let _ = vis;
                            if args.len() + 1 < arity_min || args.len() + 1 > arity {
                                let want = if arity_min == arity {
                                    format!("{} argument(s)", arity - 1)
                                } else {
                                    format!("{} to {} argument(s)", arity_min - 1, arity - 1)
                                };
                                return Err(self.err(
                                    span,
                                    format!(
                                        "method '{}' expects {}, got {}",
                                        name,
                                        want,
                                        args.len()
                                    ),
                                ));
                            }
                        }
                        None => {
                            return Err(
                                self.err(span, format!("class '{}' has no method '{}'", cls, name))
                            )
                        }
                    }
                    // argument type checking against the method signature
                    if let Some(msig) = self.method_sig(&cls, name).cloned() {
                        for (i, a) in args.iter().enumerate() {
                            if let Some(Some(pt)) = msig.params.get(i + 1) {
                                let want = self.resolve_type(pt);
                                let got = self.type_of(a);
                                if !self.assignable(&got, &want) {
                                    return Err(self.err(
                                        a.span(),
                                        format!(
                                            "argument {} of method '{}' expects {}, got {}",
                                            i + 1,
                                            name,
                                            want.name(),
                                            got.name()
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                }
                Ok(())
            }
            Expr::Index { object, index, .. } => {
                self.check_expr(object)?;
                self.check_expr(index)
            }
            Expr::Field { object, name, span } => {
                self.check_expr(object)?;
                let recv_class = self.receiver_class(object);
                if let Some(cls) = recv_class {
                    match self.field_decl(&cls, name) {
                        Some((vis, decl_cls)) => {
                            if vis == Vis::Private
                                && self.current_class.as_deref() != Some(&decl_cls)
                            {
                                return Err(self.err(
                                    span,
                                    format!("field '{}' of class '{}' is private", name, decl_cls),
                                ));
                            }
                        }
                        None => {
                            return Err(
                                self.err(span, format!("class '{}' has no field '{}'", cls, name))
                            )
                        }
                    }
                }
                Ok(())
            }
            Expr::New { class, args, span } => {
                // An imported public name (e.g. a class re-exported by a
                // module) is in scope as a variable even though its class body
                // is not in this file; accept it as constructible. Locally
                // declared classes still get the constructor-arity check here;
                // imported ones are validated at runtime.
                let known = self.classes.contains_key(class)
                    || class == "Object"
                    || self.lookup(class).is_some();
                if !known {
                    return Err(self.err(span, format!("unknown class '{}'", class)));
                }
                if self.classes.contains_key(class) {
                    if self
                        .classes
                        .get(class)
                        .map(|c| c.is_abstract)
                        .unwrap_or(false)
                    {
                        return Err(self.err(
                            span,
                            format!(
                                "cannot instantiate the abstract class '{}' (subclass it and \
                                 implement its abstract methods)",
                                class
                            ),
                        ));
                    }
                    if let Some(mi) = self.method_sig(class, "new") {
                        if Self::has_named_args(args) {
                            let names = mi.param_names.clone();
                            self.check_named_args(
                                args,
                                &names,
                                mi.min_arity,
                                &format!("new {}", class),
                                span,
                            )?;
                        } else if args.len() < mi.min_arity || args.len() > mi.arity() {
                            let want = if mi.min_arity == mi.arity() {
                                format!("{} argument(s)", mi.arity())
                            } else {
                                format!("{} to {} argument(s)", mi.min_arity, mi.arity())
                            };
                            return Err(self.err(
                                span,
                                format!("new {} expects {}, got {}", class, want, args.len()),
                            ));
                        }
                    } else if !args.is_empty() {
                        return Err(self.err(
                            span,
                            format!("new {} expects 0 argument(s), got {}", class, args.len()),
                        ));
                    }
                } else if class == "Object" && !args.is_empty() {
                    return Err(self.err(
                        span,
                        format!("new Object expects 0 argument(s), got {}", args.len()),
                    ));
                } else if Self::has_named_args(args) {
                    return Err(self.err(
                        span,
                        format!(
                            "named arguments on 'new {}' are not supported: \
                             the class is not declared in this file",
                            class
                        ),
                    ));
                }
                for a in args {
                    self.check_arg(a)?;
                }
                Ok(())
            }
            Expr::Spawn { callable, .. } => self.check_expr(callable),
            Expr::Await(inner, span) => {
                if !self.in_async {
                    return Err(self.err(span, "await is only allowed inside an async function"));
                }
                self.check_expr(inner)
            }
        }
    }
}
