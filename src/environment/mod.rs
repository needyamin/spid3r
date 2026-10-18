//! Lexical environments. Environments are `Arc<Mutex<_>>` so the same tree can
//! be shared between tasks on different OS threads (the scheduler requires
//! this; `Rc` would not be `Send`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::value::Value;

#[derive(Default)]
pub struct EnvironmentData {
    pub vars: HashMap<String, Value>,
    /// Bindings declared with `const`. Assigning to one is a runtime error
    /// (the semantic pass already rejects the statically detectable cases;
    /// this set is the defense for dynamically imported bindings).
    pub consts: HashSet<String>,
    pub parent: Option<Env>,
}

pub type Env = Arc<Mutex<EnvironmentData>>;

pub fn new_global() -> Env {
    Arc::new(Mutex::new(EnvironmentData {
        vars: HashMap::new(),
        consts: HashSet::new(),
        parent: None,
    }))
}

pub fn child(parent: &Env) -> Env {
    Arc::new(Mutex::new(EnvironmentData {
        vars: HashMap::new(),
        consts: HashSet::new(),
        parent: Some(parent.clone()),
    }))
}

pub fn child_with_vars(parent: &Env, vars: HashMap<String, Value>) -> Env {
    Arc::new(Mutex::new(EnvironmentData {
        vars,
        consts: HashSet::new(),
        parent: Some(parent.clone()),
    }))
}


impl EnvironmentData {
    /// Read a variable, walking the parent chain. Clones the value out, so no
    /// lock is held while user code runs.
    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.vars.get(name) {
            return Some(v.clone());
        }
        self.parent.as_ref()?.lock().unwrap().get(name)
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
            return Ok(());
        }
        match &self.parent {
            Some(p) => p.lock().unwrap().assign(name, value),
            None => Err(AssignError::Unbound(name.to_string())),
        }
    }

    /// Define in the global (root) environment, whatever `self` is.
    pub fn set_global(env: &Env, name: &str, value: Value) {
        let parent = env.lock().unwrap().parent.clone();
        match parent {
            Some(p) => Self::set_global(&p, name, value),
            None => {
                env.lock().unwrap().vars.insert(name.to_string(), value);
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
    env.lock().unwrap().get(name)
}

pub fn define(env: &Env, name: &str, value: Value) {
    env.lock().unwrap().vars.insert(name.to_string(), value);
}

/// Define an immutable (`const`) binding in the current environment.
pub fn define_const(env: &Env, name: &str, value: Value) {
    let mut e = env.lock().unwrap();
    e.vars.insert(name.to_string(), value);
    e.consts.insert(name.to_string());
}

/// Mark an existing binding (defined elsewhere, e.g. by a module import) as
/// `const` in the environment that now holds it. No-op when unbound.
pub fn mark_const(env: &Env, name: &str) {
    let mut e = env.lock().unwrap();
    if e.vars.contains_key(name) {
        e.consts.insert(name.to_string());
    }
}

/// Assign through the chain. `Err` carries the reason: a `const` violation or
/// an unbound name (the caller turns that into its usual diagnostic).
pub fn assign(env: &Env, name: &str, value: Value) -> Result<(), AssignError> {
    env.lock().unwrap().assign(name, value)
}
