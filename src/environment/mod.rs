//! Lexical environments. Environments are `Arc<Mutex<_>>` so the same tree can
//! be shared between tasks on different OS threads (the scheduler requires
//! this; `Rc` would not be `Send`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::value::collections::{FxHashMap, FxHashSet};
use crate::value::gc::{self, GcTrace};
use crate::value::{lock_ref, Value};

/// Generation counter bumped whenever any root/global environment is mutated
/// (Track 5 TW-2). Lets the tree-walk interpreter cache resolved global/builtin
/// lookups without locking the global environment on every loop iteration.
static ENV_GLOBAL_GEN: AtomicU64 = AtomicU64::new(1);

#[inline]
pub fn global_gen() -> u64 {
    ENV_GLOBAL_GEN.load(Ordering::Relaxed)
}

#[inline]
pub fn bump_global_gen() {
    ENV_GLOBAL_GEN.fetch_add(1, Ordering::Relaxed);
}

#[derive(Default)]
pub struct EnvironmentData {
    pub vars: FxHashMap<String, Value>,
    /// Bindings declared with `const`. Assigning to one is a runtime error
    /// (the semantic pass already rejects the statically detectable cases;
    /// this set is the defense for dynamically imported bindings).
    pub consts: FxHashSet<String>,
    pub parent: Option<Env>,
    /// True only on root environments created via [`new_global`], enabling
    /// lazy fallback to builtin natives without duplicating ~230 entries upfront.
    pub is_global: bool,
}

pub type Env = Arc<Mutex<EnvironmentData>>;

pub fn new_global() -> Env {
    bump_global_gen();
    // Every environment is a tracked cell (audit A-13): a closure bound in
    // the scope it captures forms `env -> closure -> env`, and the collector
    // can only reclaim it when both ends are cells.
    gc::track(Arc::new(Mutex::new(EnvironmentData {
        vars: FxHashMap::default(),
        consts: FxHashSet::default(),
        parent: None,
        is_global: true,
    })))
}

pub fn child(parent: &Env) -> Env {
    gc::track(Arc::new(Mutex::new(EnvironmentData {
        vars: FxHashMap::default(),
        consts: FxHashSet::default(),
        parent: Some(parent.clone()),
        is_global: false,
    })))
}

pub fn child_with_vars(parent: &Env, vars: FxHashMap<String, Value>) -> Env {
    gc::track(Arc::new(Mutex::new(EnvironmentData {
        vars,
        consts: FxHashSet::default(),
        parent: Some(parent.clone()),
        is_global: false,
    })))
}

impl GcTrace for EnvironmentData {
    fn gc_trace(&self, f: &mut dyn FnMut(&Value)) {
        for v in self.vars.values() {
            f(v);
        }
    }
    fn gc_trace_cells(&self, f: &mut dyn FnMut(usize)) {
        if let Some(p) = &self.parent {
            f(Arc::as_ptr(p) as *const () as usize);
        }
    }
    fn gc_clear(&mut self) {
        self.vars.clear();
    }
}

impl EnvironmentData {
    /// Read a variable, walking the parent chain. Clones the value out, so no
    /// lock is held while user code runs.
    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.vars.get(name) {
            return Some(v.clone());
        }
        match &self.parent {
            Some(p) => lock_ref(p).get(name),
            None if self.is_global => {
                crate::stdlib::resolve_builtin_native(name).map(Value::Native)
            }
            None => None,
        }
    }

    /// Assign to an existing binding, walking the parent chain.
    /// `Err(AssignError::Const)` is a `const` violation; `Err(AssignError::Unbound)`
    /// means the name is not bound anywhere.
    pub fn assign(&mut self, name: &str, value: Value) -> Result<(), AssignError> {
        if let Some(slot) = self.vars.get_mut(name) {
            if !self.consts.is_empty() && self.consts.contains(name) {
                return Err(AssignError::Const(name.to_string()));
            }
            *slot = value;
            if self.is_global {
                bump_global_gen();
            }
            return Ok(());
        }
        match &self.parent {
            Some(p) => lock_ref(p).assign(name, value),
            None if self.is_global && crate::stdlib::resolve_builtin_native(name).is_some() => {
                self.vars.insert(name.to_string(), value);
                bump_global_gen();
                Ok(())
            }
            None => Err(AssignError::Unbound(name.to_string())),
        }
    }

    /// Define in the global (root) environment, whatever `self` is.
    pub fn set_global(env: &Env, name: &str, value: Value) {
        let parent = lock_ref(env).parent.clone();
        match parent {
            Some(p) => Self::set_global(&p, name, value),
            None => {
                bump_global_gen();
                lock_ref(env).vars.insert(name.to_string(), value);
            }
        }
    }
}

/// Why an assignment failed.
#[derive(Debug, Clone)]
pub enum AssignError {
    /// The binding exists but was declared `const`.
    Const(String),
    /// The name is not bound in any visible scope.
    Unbound(String),
}

pub fn get(env: &Env, name: &str) -> Option<Value> {
    lock_ref(env).get(name)
}

pub fn define(env: &Env, name: &str, value: Value) {
    let mut e = lock_ref(env);
    if e.is_global {
        bump_global_gen();
    }
    e.vars.insert(name.to_string(), value);
}

/// Define an immutable (`const`) binding in the current environment.
pub fn define_const(env: &Env, name: &str, value: Value) {
    let mut e = lock_ref(env);
    if e.is_global {
        bump_global_gen();
    }
    e.vars.insert(name.to_string(), value);
    e.consts.insert(name.to_string());
}

/// Mark an existing binding (defined elsewhere, e.g. by a module import) as
/// `const` in the environment that now holds it. No-op when unbound.
pub fn mark_const(env: &Env, name: &str) {
    let mut e = lock_ref(env);
    if e.vars.contains_key(name) {
        e.consts.insert(name.to_string());
    }
}

/// Assign through the chain. `Err` carries the reason: a `const` violation or
/// an unbound name (the caller turns that into its usual diagnostic).
pub fn assign(env: &Env, name: &str, value: Value) -> Result<(), AssignError> {
    lock_ref(env).assign(name, value)
}
