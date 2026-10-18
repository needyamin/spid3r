//! Static types for the optional (gradual) type checker.
//!
//! This is a *compile-time-only* layer. It never changes runtime behaviour:
//! an unannotated binding has type `Any` (its value is unchecked), and an
//! `any` annotation also means "do not check". Only when a binding, parameter,
//! field, or return position carries a concrete annotation do we compare the
//! computed static type of the value against it.

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
    /// A function value (lambda or named function). Not deeply typed yet.
    Fn,
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
            Ty::Fn => "function".to_string(),
        }
    }
}
