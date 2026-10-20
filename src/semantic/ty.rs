//! Static types for the optional (gradual) type checker.
//!
//! This is a *compile-time-only* layer. It never changes runtime behaviour:
//! an unannotated binding has type `Any` (its value is unchecked), and an
//! `any` annotation also means "do not check". Only when a binding, parameter,
//! field, or return position carries a concrete annotation do we compare the
//! computed static type of the value against it.

use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    /// Dynamically typed: never checked. The default for unannotated names and
    /// for `any` annotations.
    Any,
    Int,
    Float,
    Bool,
    Str,
    Null,
    /// A collection or other nominal type. The string is the (lowercased)
    /// builtin/collection name (`list`, `map`, `option`, ...). The optional
    /// element type is tracked best-effort for diagnostics but is *erased* when
    /// deciding assignability (gradual typing does not deeply check element
    /// types in this version).
    Coll(String, Option<Box<Ty>>),
    Class(String),
    Interface(String),
    /// A function value (lambda or named function). When its parameter/return
    /// annotations are known, the carried signature lets the checker validate
    /// calls through let-bound functions, nested declarations, and lambdas —
    /// not only direct calls to the declaration name (audit F5). `None` keeps
    /// the fully dynamic behavior for unannotated functions.
    Fn(Option<Arc<FnSig>>),
}

/// Parameter/return signature carried by [`Ty::Fn`] for functions whose
/// annotations are known. Types are already resolved against the class
/// registry at construction time.
#[derive(Clone, Debug, PartialEq)]
pub struct FnSig {
    /// Number of parameters.
    pub arity: usize,
    /// Resolved per-parameter type; `None` = unannotated (unchecked).
    pub params: Vec<Option<Ty>>,
    /// Resolved return type, if annotated.
    pub ret: Option<Ty>,
}

impl Ty {
    /// A short, user-facing name for diagnostics.
    pub fn name(&self) -> String {
        match self {
            Ty::Any => "any".to_string(),
            Ty::Int => "int".to_string(),
            Ty::Float => "float".to_string(),
            Ty::Bool => "bool".to_string(),
            Ty::Str => "string".to_string(),
            Ty::Null => "null".to_string(),
            Ty::Coll(n, _) => n.clone(),
            Ty::Class(c) => c.clone(),
            Ty::Interface(i) => i.clone(),
            Ty::Fn(_) => "function".to_string(),
        }
    }

    /// Like [`Ty::name`], but a function type carries its signature
    /// (`fn(int, string) -> int`). Used in diagnostics where "function" on both
    /// sides of a mismatch would say nothing (audit A-10).
    pub fn detail_name(&self) -> String {
        match self {
            Ty::Fn(Some(sig)) => {
                let params: Vec<String> = sig
                    .params
                    .iter()
                    .map(|p| {
                        p.as_ref()
                            .map(|t| t.name())
                            .unwrap_or_else(|| "any".to_string())
                    })
                    .collect();
                let ret = sig
                    .ret
                    .as_ref()
                    .map(|t| t.name())
                    .unwrap_or_else(|| "any".to_string());
                format!("fn({}) -> {}", params.join(", "), ret)
            }
            other => other.name(),
        }
    }
}
