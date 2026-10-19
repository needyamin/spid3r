//! Semantic pass: name resolution, scope rules, privacy, interface
//! conformance, arity of statically known callees, a `main` requirement, and
//! optional *gradual* type checking (`spid3r check` enforces `: Type`
//! annotations; unannotated names stay dynamically typed).

mod ty;
use ty::Ty;

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diagnostics::{Diagnostic, Phase, Span, SpidResult};

/// Signature of a declared method, retained so call sites and `return`s inside
/// the method body can be type-checked.
#[derive(Clone)]
struct MethodInfo {
    vis: Vis,
    /// Parameter types, including `self` at index 0.
    params: Vec<Option<TypeExpr>>,
    ret: Option<TypeExpr>,
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
    params: Vec<Option<TypeExpr>>,
    ret: Option<TypeExpr>,
}

struct ClassInfo {
    parent: Option<String>,
    interfaces: Vec<String>,
    fields: HashMap<String, (Vis, Option<TypeExpr>)>,
    methods: HashMap<String, MethodInfo>,
}

struct VarInfo {
    is_const: bool,
    class: Option<String>,
    /// Static type of the binding. `None` means dynamically typed (`Any`).
    ty: Option<Ty>,
}

struct Checker {
    classes: HashMap<String, ClassInfo>,
    interfaces: HashMap<String, HashMap<String, usize>>,
    funcs: HashMap<String, FuncInfo>,
    scopes: Vec<HashMap<String, VarInfo>>,
    loop_depth: usize,
    fn_depth: usize,
    in_async: bool,
    current_class: Option<String>,
    current_ret: Option<Ty>,
    is_main: bool,
    source_file: String,
}

pub fn check(program: &Program, is_main: bool) -> SpidResult<()> {
    check_file(program, is_main, "<memory>")
}

pub fn check_file(program: &Program, is_main: bool, source_file: &str) -> SpidResult<()> {
    let mut c = Checker::new(is_main, source_file);
    c.check_program(program)
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
    let mut c = Checker::new(false, "<repl>");
    for n in known {
        c.scopes[0].insert(
            n.clone(),
            VarInfo {
                is_const: false,
                class: None,
                ty: None,
            },
        );
    }
    c.check_program(program)
}

impl Checker {
    fn new(is_main: bool, source_file: &str) -> Checker {
        Checker {
            classes: HashMap::new(),
            interfaces: HashMap::new(),
            funcs: HashMap::new(),
            scopes: vec![HashMap::new()],
            loop_depth: 0,
            fn_depth: 0,
            in_async: false,
            current_class: None,
            current_ret: None,
            is_main,
            source_file: source_file.to_string(),
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
    "file_exists",
    "sleep_ms",
    "now",
    "env",
    "args",
    "spawn_process",
    "yield",
    "task_count",
    "len",
    "abs",
    "sqrt",
    "pow",
    "min",
    "max",
    "int",
    "float",
    "string",
    "bool",
    "random_int",
    "json_parse",
    "json_write",
    "type_of",
    "gc_collect",
    "gc_live_cells",
    // networking (src/stdlib/net.rs)
    "tcp_listen",
    "tcp_accept",
    "tcp_connect",
    "tcp_read",
    "tcp_write",
    "tcp_close",
    "tcp_local_addr",
    "tcp_peer_addr",
    "tcp_set_timeout",
    "http_read_request",
    "http_write_response",
    "http_fetch",
    "url_decode",
    "url_encode",
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
        "int" | "len" | "abs" | "random_int" | "now" => Ty::Int,
        "float" | "sqrt" | "pow" => Ty::Float,
        "string" => Ty::Str,
        "bool" => Ty::Bool,
        "type_of" => Ty::Str,
        _ => return None,
    })
}

impl Checker {
    fn err(&self, span: &Span, msg: impl Into<String>) -> Diagnostic {
        Diagnostic::at(Phase::Semantic, span.clone(), msg)
    }

    fn check_program(&mut self, program: &Program) -> SpidResult<()> {
        // pass 1: collect top-level names
        let mut seen = HashSet::new();
        for s in &program.stmts {
            match s {
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
                    self.funcs.insert(
                        name.clone(),
                        FuncInfo {
                            arity: f.params.len(),
                            params: f.params.iter().map(|p| p.ty.clone()).collect(),
                            ret: f.ret.clone(),
                        },
                    );
                    self.declare(
                        &name,
                        &f.span,
                        VarInfo {
                            is_const: false,
                            class: None,
                            ty: None,
                        },
                    )?;
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
                                ret: m.ret.clone(),
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
                    },
                );
            }
        }
        for s in &program.stmts {
            if let Stmt::Interface(i) = s {
                let mut methods = HashMap::new();
                for (name, params, _, span) in &i.methods {
                    if params.first().map(|p| p.name.as_str()) != Some("self") {
                        return Err(
                            self.err(span, format!("interface method '{}' must take self", name))
                        );
                    }
                    methods.insert(name.clone(), params.len());
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
                    for (mname, arity) in methods {
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
            self.check_stmt(s)?;
        }
        Ok(())
    }

    fn declare(&mut self, name: &str, span: &Span, info: VarInfo) -> SpidResult<()> {
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

    fn known(&self, name: &str) -> bool {
        self.lookup(name).is_some()
            || self.funcs.contains_key(name)
            || self.classes.contains_key(name)
            || self.interfaces.contains_key(name)
            || name == "Object"
            || name == "none"
            || BUILTIN_CONSTRUCTORS.contains(&name)
            || BUILTIN_FUNCTIONS.contains(&name)
            || gui_builtins().contains(&name)
    }

    /// Field visibility through the inheritance chain.
    fn field_vis(&self, class: &str, field: &str) -> Option<Vis> {
        let mut cur = Some(class.to_string());
        let mut seen = HashSet::new();
        while let Some(c) = cur {
            if !seen.insert(c.clone()) {
                return None; // cycle guard
            }
            let info = self.classes.get(&c)?;
            if let Some((vis, _)) = info.fields.get(field) {
                return Some(*vis);
            }
            cur = info.parent.clone();
        }
        None
    }

    /// Method (visibility, arity) through the inheritance chain.
    fn method_info(&self, class: &str, method: &str) -> Option<(Vis, usize)> {
        self.method_sig(class, method).map(|m| (m.vis, m.arity()))
    }

    /// Full method signature through the inheritance chain.
    fn method_sig(&self, class: &str, method: &str) -> Option<&MethodInfo> {
        let mut cur = Some(class.to_string());
        let mut seen = HashSet::new();
        while let Some(c) = cur {
            if !seen.insert(c.clone()) {
                return None; // cycle guard
            }
            let ci = self.classes.get(&c)?;
            if let Some(m) = ci.methods.get(method) {
                return Some(m);
            }
            cur = ci.parent.clone();
        }
        None
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn check_block(&mut self, b: &Block) -> SpidResult<()> {
        self.push_scope();
        let r = self.check_stmts(&b.stmts);
        self.pop_scope();
        r
    }

    fn check_stmts(&mut self, stmts: &[Stmt]) -> SpidResult<()> {
        for s in stmts {
            self.check_stmt(s)?;
        }
        Ok(())
    }

    fn check_stmt(&mut self, s: &Stmt) -> SpidResult<()> {
        match s {
            Stmt::Import { path, span } => {
                let names = crate::modules::public_names_of(path, &self.source_file, span)?;
                let scope = self.scopes.last_mut().unwrap();
                for (n, is_const) in names {
                    scope.insert(
                        n,
                        VarInfo {
                            is_const,
                            class: None,
                            ty: None,
                        },
                    );
                }
                Ok(())
            }
            Stmt::Fn(f) => {
                if f.params.iter().any(|p| p.name == "self") {
                    return Err(self.err(&f.span, "self is only allowed in methods"));
                }
                if self.scopes.len() > 1 {
                    if let Some(name) = &f.name {
                        self.declare(
                            name,
                            &f.span,
                            VarInfo {
                                is_const: false,
                                class: None,
                                ty: Some(Ty::Fn),
                            },
                        )?;
                    }
                }
                self.fn_depth += 1;
                let was_async = self.in_async;
                self.in_async = f.is_async;
                self.push_scope();
                let mut names = HashSet::new();
                for p in &f.params {
                    if !names.insert(p.name.clone()) {
                        return Err(self.err(&p.span, format!("duplicate parameter '{}'", p.name)));
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
                        },
                    )?;
                }
                if let Some(ret) = &f.ret {
                    self.check_type_name(&ret.name, &ret.span)?;
                }
                let saved_ret = self.current_ret.take();
                self.current_ret = f.ret.as_ref().map(|t| self.resolve_type(t));
                let r = self.check_stmts(&f.body.stmts);
                self.current_ret = saved_ret;
                self.pop_scope();
                self.in_async = was_async;
                self.fn_depth -= 1;
                r
            }
            Stmt::Class(c) => {
                self.current_class = Some(c.name.clone());
                // method bodies: self + fields in scope
                for m in &c.methods {
                    self.fn_depth += 1;
                    let was_async = self.in_async;
                    self.in_async = m.is_async;
                    self.push_scope();
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
                            },
                        )?;
                        let mut names = HashSet::new();
                        for p in &m.params {
                            if !names.insert(p.name.clone()) {
                                return Err(self.err(
                                    &p.span,
                                    format!("duplicate parameter '{}'", p.name),
                                ));
                            }
                            self.declare(
                                &p.name,
                                &p.span,
                                VarInfo {
                                    is_const: false,
                                    class: None,
                                    ty: p.ty.as_ref().map(|t| self.resolve_type(t)),
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
                            },
                        )?;
                        let mut names = HashSet::new();
                        for p in m.params.iter().skip(1) {
                            if !names.insert(p.name.clone()) {
                                return Err(self.err(
                                    &p.span,
                                    format!("duplicate parameter '{}'", p.name),
                                ));
                            }
                            self.declare(
                                &p.name,
                                &p.span,
                                VarInfo {
                                    is_const: false,
                                    class: None,
                                    ty: p.ty.as_ref().map(|t| self.resolve_type(t)),
                                },
                            )?;
                        }
                    }
                    let saved_ret = self.current_ret.take();
                    self.current_ret = m.ret.as_ref().map(|t| self.resolve_type(t));
                    let r = self.check_stmts(&m.body.stmts);
                    self.current_ret = saved_ret;
                    self.pop_scope();
                    self.in_async = was_async;
                    self.fn_depth -= 1;
                    r?;
                }
                self.current_class = None;
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
                        return Err(self.err(
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
                self.declare(
                    name,
                    span,
                    VarInfo {
                        is_const: false,
                        class,
                        ty: ty.as_ref().map(|t| self.resolve_type(t)),
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
                        return Err(self.err(
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
                self.declare(
                    name,
                    span,
                    VarInfo {
                        is_const: true,
                        class: None,
                        ty: ty.as_ref().map(|t| self.resolve_type(t)),
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
                var, iter, body, ..
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
                    },
                )?;
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
                            return Err(self.err(
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
                            return Err(self.err(
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
            Stmt::Spawn { callable, span } => {
                self.check_expr(callable)?;
                if !matches!(callable, Expr::Fn(_)) {
                    return Err(self.err(span, "spawn expects a function: spawn fn() { ... };"));
                }
                Ok(())
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
            "list" | "vec" | "stack" | "queue" | "deque" | "linked_list"
            | "singly_linked_list" | "doubly_linked_list" | "map" | "set" | "tuple"
            | "binary_tree" | "bst" | "heap" | "priority_queue" | "graph" | "hash_table"
            | "trie" | "matrix" | "ring_buffer" | "bag" | "ref" | "option" | "result"
            | "task" | "channel" | "mutex" | "some" | "none" | "ok" | "err" => {
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
    fn type_of(&self, e: &Expr) -> Ty {
        match e {
            Expr::Int(_, _) => Ty::Int,
            Expr::Float(_, _) => Ty::Float,
            Expr::Str(_, _) => Ty::Str,
            Expr::Bool(_, _) => Ty::Bool,
            Expr::Null(_) => Ty::Null,
            Expr::Ident(name, _) => {
                if name == "self" {
                    self.current_class
                        .clone()
                        .map(Ty::Class)
                        .unwrap_or(Ty::Any)
                } else {
                    self.lookup(name).and_then(|v| v.ty.clone()).unwrap_or(Ty::Any)
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
            Expr::Fn(_) => Ty::Fn,
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
                        return Ty::Coll(name.to_string(), elem.map(Box::new));
                    }
                    if let Some(rt) = builtin_return_type(name) {
                        return rt;
                    }
                }
                Ty::Any
            }
            Expr::Method { object, name, .. } => {
                let recv = self.type_of(object);
                if let Ty::Class(c) = recv {
                    if let Some(mi) = self.method_sig(&c, name) {
                        return mi.ret.as_ref().map(|t| self.resolve_type(t)).unwrap_or(Ty::Any);
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
            (Ty::Coll(a, _), Ty::Coll(b, _)) => a == b,
            (Ty::Fn, Ty::Fn) => true,
            _ => false,
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
        {
            Ok(())
        } else {
            Err(self.err(span, format!("unknown type '{}'", name)))
        }
    }

    fn check_expr(&mut self, e: &Expr) -> SpidResult<()> {
        match e {
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
                    Ok(())
                } else {
                    Err(self.err(span, format!("unknown name '{}'", name)))
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
                self.push_scope();
                let mut names = HashSet::new();
                for p in &f.params {
                    if !names.insert(p.name.clone()) {
                        return Err(self.err(&p.span, format!("duplicate parameter '{}'", p.name)));
                    }
                    self.declare(
                        &p.name,
                        &p.span,
                        VarInfo {
                            is_const: false,
                            class: None,
                            ty: p.ty.as_ref().map(|t| self.resolve_type(t)),
                        },
                    )?;
                }
                let saved_ret = self.current_ret.take();
                self.current_ret = f.ret.as_ref().map(|t| self.resolve_type(t));
                let r = self.check_stmts(&f.body.stmts);
                self.current_ret = saved_ret;
                self.pop_scope();
                self.in_async = was_async;
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
            Expr::Assign { target, value, span, .. } => {
                match &**target {
                    Expr::Ident(name, _) => match self.lookup(name) {
                        Some(v) if v.is_const => {
                            return Err(self.err(span, format!("cannot assign to const '{}'", name)))
                        }
                        None if !self.known(name) => {
                            return Err(self.err(span, format!("unknown name '{}'", name)))
                        }
                        Some(v) => {
                            if let Some(t) = &v.ty {
                                let got = self.type_of(value);
                                if !self.assignable(&got, t) {
                                    return Err(self.err(
                                        span,
                                        format!(
                                            "type mismatch assigning to '{}': expected {}, got {}",
                                            name,
                                            t.name(),
                                            got.name()
                                        ),
                                    ));
                                }
                            }
                        }
                        _ => {}
                    },
                    Expr::Index { object, .. } => self.check_expr(object)?,
                    Expr::Field { object, name, .. } => {
                        self.check_expr(object)?;
                        let recv = self.type_of(object);
                        if let Ty::Class(c) = recv {
                            if let Some((_, Some(ft))) =
                                self.classes.get(&c).and_then(|ci| ci.fields.get(name))
                            {
                                let want = self.resolve_type(ft);
                                let got = self.type_of(value);
                                if !self.assignable(&got, &want) {
                                    return Err(self.err(
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
            Expr::Call { callee, args, span } => {
                if let Expr::Ident(name, _) = &**callee {
                    if let Some(fi) = self.funcs.get(name).cloned() {
                        if args.len() != fi.arity {
                            return Err(self.err(
                                span,
                                format!(
                                    "{} expects {} argument(s), got {}",
                                    name,
                                    fi.arity,
                                    args.len()
                                ),
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
                    } else if !self.known(name) {
                        return Err(self.err(span, format!("unknown name '{}'", name)));
                    }
                } else {
                    self.check_expr(callee)?;
                }
                for a in args {
                    self.check_expr(a)?;
                }
                Ok(())
            }
            Expr::Method {
                object,
                name,
                args,
                span,
            } => {
                self.check_expr(object)?;
                for a in args {
                    self.check_expr(a)?;
                }
                // when the receiver class is statically known, check it
                let recv_class = match &**object {
                    Expr::Ident(n, _) => self.lookup(n).and_then(|v| v.class.clone()),
                    _ => None,
                };
                if let Some(cls) = recv_class {
                    match self.method_info(&cls, name) {
                        Some((vis, arity)) => {
                            if vis == Vis::Private && self.current_class.as_deref() != Some(&cls) {
                                return Err(self.err(
                                    span,
                                    format!("method '{}' of class '{}' is private", name, cls),
                                ));
                            }
                            if args.len() + 1 != arity {
                                return Err(self.err(
                                    span,
                                    format!(
                                        "method '{}' expects {} argument(s), got {}",
                                        name,
                                        arity - 1,
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
                let recv_class = match &**object {
                    Expr::Ident(n, _) if n == "self" => self.current_class.clone(),
                    Expr::Ident(n, _) => self.lookup(n).and_then(|v| v.class.clone()),
                    _ => None,
                };
                if let Some(cls) = recv_class {
                    match self.field_vis(&cls, name) {
                        Some(vis) => {
                            if vis == Vis::Private && self.current_class.as_deref() != Some(&cls) {
                                return Err(self.err(
                                    span,
                                    format!("field '{}' of class '{}' is private", name, cls),
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
                    if let Some(mi) = self.method_sig(class, "new") {
                        if args.len() != mi.arity() {
                            return Err(self.err(
                                span,
                                format!(
                                    "new {} expects {} argument(s), got {}",
                                    class,
                                    mi.arity(),
                                    args.len()
                                ),
                            ));
                        }
                    } else if !args.is_empty() {
                        return Err(self.err(
                            span,
                            format!("new {} expects 0 argument(s), got {}", class, args.len()),
                        ));
                    }
                }
                for a in args {
                    self.check_expr(a)?;
                }
                Ok(())
            }
            Expr::Spawn { callable, span } => {
                self.check_expr(callable)?;
                if !matches!(callable.as_ref(), Expr::Fn(_)) {
                    return Err(self.err(span, "spawn expects a function: spawn fn() { ... };"));
                }
                Ok(())
            }
            Expr::Await(inner, span) => {
                if !self.in_async {
                    return Err(self.err(span, "await is only allowed inside an async function"));
                }
                self.check_expr(inner)
            }
        }
    }
}
