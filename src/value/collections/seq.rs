//! Sequence collections: list, vec, stack, queue, deque, map, set.
//! All of these are real structures with the documented semantics; a list is
//! an indexed vector, a stack is strict LIFO on a vector, a queue is strict
//! FIFO on a ring-buffer deque.

use std::collections::VecDeque;

use super::{deep_clone, key_to_value, new_ref, values_equal, Value};

// ---------------------------------------------------------------- list, vec

#[derive(Debug, Default)]
pub struct VecData {
    pub items: Vec<Value>,
}

impl Drop for VecData {
    fn drop(&mut self) {
        // F4: a deeply nested, singly-owned list chain (a list containing a
        // list containing ...) overflows the host stack with the default
        // recursive drop at ~10^5 levels — the teardown of a deep build
        // crashed the host after the build stall was fixed. Drain the chain
        // iteratively instead: when a child list's cell is the LAST owner,
        // take its items into our worklist rather than letting its own Drop
        // recurse. Shared cells (strong_count > 1) fall back to the normal
        // drop — their recursion depth is bounded by the sharing structure.
        let mut work = std::mem::take(&mut self.items);
        while let Some(v) = work.pop() {
            let sole = match &v {
                Value::List(arc) | Value::VecDyn(arc) => {
                    std::sync::Arc::strong_count(arc) == 1
                }
                _ => false,
            };
            if !sole {
                drop(v);
                continue;
            }
            match v {
                Value::List(arc) | Value::VecDyn(arc) => {
                    if let Ok(data) = std::sync::Arc::try_unwrap(arc) {
                        let mut data = data.into_inner().unwrap_or_else(|e| e.into_inner());
                        work.extend(std::mem::take(&mut data.items));
                    }
                }
                _ => drop(v),
            }
        }
    }
}

impl VecData {
    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.items.len() as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.items.is_empty()))
            }
            "clear" => {
                want(name, args, 0)?;
                self.items.clear();
                Ok(Value::Null)
            }
            "capacity" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.items.capacity() as i64))
            }
            "push" => {
                want(name, args, 1)?;
                self.items.push(args[0].clone());
                Ok(Value::Null)
            }
            "pop" => {
                want(name, args, 0)?;
                match self.items.pop() {
                    Some(v) => Ok(v),
                    None => Err("pop on an empty list".to_string()),
                }
            }
            "peek" => {
                want(name, args, 0)?;
                match self.items.last() {
                    Some(v) => Ok(v.clone()),
                    None => Err("peek on an empty list".to_string()),
                }
            }
            "get" => {
                want(name, args, 1)?;
                let i = want_index(&args[0], "list.get")?;
                match self.items.get(i) {
                    Some(v) => Ok(v.clone()),
                    None => Err(format!(
                        "index {} out of bounds for length {}",
                        i,
                        self.items.len()
                    )),
                }
            }
            "set" => {
                want(name, args, 2)?;
                let i = want_index(&args[0], "list.set")?;
                match self.items.get_mut(i) {
                    Some(slot) => {
                        *slot = args[1].clone();
                        Ok(Value::Null)
                    }
                    None => Err(format!(
                        "index {} out of bounds for length {}",
                        i,
                        self.items.len()
                    )),
                }
            }
            "insert" => {
                want(name, args, 2)?;
                let i = want_index(&args[0], "list.insert")?;
                if i > self.items.len() {
                    return Err(format!(
                        "index {} out of bounds for insert at length {}",
                        i,
                        self.items.len()
                    ));
                }
                self.items.insert(i, args[1].clone());
                Ok(Value::Null)
            }
            "remove_at" => {
                want(name, args, 1)?;
                let i = want_index(&args[0], "list.remove_at")?;
                if i >= self.items.len() {
                    return Err(format!(
                        "index {} out of bounds for length {}",
                        i,
                        self.items.len()
                    ));
                }
                Ok(self.items.remove(i))
            }
            "remove" => {
                want(name, args, 1)?;
                let pos = self.items.iter().position(|v| values_equal(v, &args[0]));
                match pos {
                    Some(i) => Ok(self.items.remove(i)),
                    None => Err("value not in list".to_string()),
                }
            }
            "index_of" => {
                want(name, args, 1)?;
                let pos = self.items.iter().position(|v| values_equal(v, &args[0]));
                Ok(Value::Int(pos.map(|p| p as i64).unwrap_or(-1)))
            }
            "contains" => {
                want(name, args, 1)?;
                Ok(Value::Bool(
                    self.items.iter().any(|v| values_equal(v, &args[0])),
                ))
            }
            "reverse" => {
                want(name, args, 0)?;
                self.items.reverse();
                Ok(Value::Null)
            }
            "sort" => {
                want(name, args, 0)?;
                let mut vals = std::mem::take(&mut self.items);
                let all_num = vals
                    .iter()
                    .all(|v| matches!(v, Value::Int(_) | Value::Float(_)));
                let all_str = vals.iter().all(|v| matches!(v, Value::Str(_)));
                if !(all_num || all_str) {
                    self.items = vals;
                    return Err(
                        "cannot sort: values are not all numbers or all strings".to_string()
                    );
                }
                vals.sort_by(|a, b| value_cmp(a, b).unwrap_or(std::cmp::Ordering::Equal));
                self.items = vals;
                Ok(Value::Null)
            }
            "slice" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(format!(
                        "slice expects 1 or 2 arguments, got {}",
                        args.len()
                    ));
                }
                let a = want_index(&args[0], "list.slice")?;
                let b = if args.len() == 2 {
                    want_index(&args[1], "list.slice")?
                } else {
                    self.items.len()
                };
                if a > b || b > self.items.len() {
                    return Err(format!("slice range {}..{} out of bounds", a, b));
                }
                Ok(Value::List(new_ref(VecData {
                    items: self.items[a..b].iter().map(deep_clone).collect(),
                })))
            }
            "join" => {
                want(name, args, 1)?;
                let sep = want_str(&args[0], "list.join")?;
                let parts: Result<Vec<String>, String> = self
                    .items
                    .iter()
                    .map(|v| match v {
                        Value::Str(s) => Ok(s.to_string()),
                        other => Err(format!(
                            "join needs strings, found {}",
                            super::type_name(other)
                        )),
                    })
                    .collect();
                Ok(Value::Str(parts?.join(&sep).into()))
            }
            "append" => {
                want(name, args, 1)?;
                let other = match &args[0] {
                    Value::List(d) | Value::VecDyn(d) => {
                        let d = d.lock().unwrap();
                        d.items.clone()
                    }
                    other => {
                        return Err(format!(
                            "append expects a list, found {}",
                            super::type_name(other)
                        ))
                    }
                };
                self.items.extend(other);
                Ok(Value::Null)
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::List(new_ref(VecData {
                    items: self.items.iter().map(deep_clone).collect(),
                })))
            }
            "sorted" => {
                want(name, args, 0)?;
                let mut vals = self.items.clone();
                let all_num = vals
                    .iter()
                    .all(|v| matches!(v, Value::Int(_) | Value::Float(_)));
                let all_str = vals.iter().all(|v| matches!(v, Value::Str(_)));
                if !(all_num || all_str) {
                    return Err(
                        "cannot sort: values are not all numbers or all strings".to_string()
                    );
                }
                vals.sort_by(|a, b| value_cmp(a, b).unwrap_or(std::cmp::Ordering::Equal));
                Ok(Value::List(new_ref(VecData { items: vals })))
            }
            "reversed" => {
                want(name, args, 0)?;
                let mut vals = self.items.clone();
                vals.reverse();
                Ok(Value::List(new_ref(VecData { items: vals })))
            }
            _ => Err(unknown(name, "list")),
        }
    }
}

// ------------------------------------------------------------------- stack

#[derive(Debug, Default)]
pub struct StackData {
    /// Bottom first, top last.
    pub items: Vec<Value>,
}

impl StackData {
    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.items.len() as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.items.is_empty()))
            }
            "clear" => {
                want(name, args, 0)?;
                self.items.clear();
                Ok(Value::Null)
            }
            "push" => {
                want(name, args, 1)?;
                self.items.push(args[0].clone());
                Ok(Value::Null)
            }
            "pop" => {
                want(name, args, 0)?;
                match self.items.pop() {
                    Some(v) => Ok(v),
                    None => Err("pop on an empty stack".to_string()),
                }
            }
            "peek" => {
                want(name, args, 0)?;
                match self.items.last() {
                    Some(v) => Ok(v.clone()),
                    None => Err("peek on an empty stack".to_string()),
                }
            }
            "contains" => {
                want(name, args, 1)?;
                Ok(Value::Bool(
                    self.items.iter().any(|v| values_equal(v, &args[0])),
                ))
            }
            "to_list" => {
                want(name, args, 0)?;
                Ok(Value::List(new_ref(VecData {
                    items: self.items.iter().map(deep_clone).collect(),
                })))
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::Stack(new_ref(StackData {
                    items: self.items.iter().map(deep_clone).collect(),
                })))
            }
            _ => Err(unknown(name, "stack")),
        }
    }
}

// ------------------------------------------------------------- queue, deque

#[derive(Debug, Default)]
pub struct QueueData {
    pub items: VecDeque<Value>,
}

#[derive(Debug, Default)]
pub struct DequeData {
    pub items: VecDeque<Value>,
}

impl QueueData {
    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.items.len() as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.items.is_empty()))
            }
            "clear" => {
                want(name, args, 0)?;
                self.items.clear();
                Ok(Value::Null)
            }
            "push" => {
                want(name, args, 1)?;
                self.items.push_back(args[0].clone());
                Ok(Value::Null)
            }
            "pop" => {
                want(name, args, 0)?;
                match self.items.pop_front() {
                    Some(v) => Ok(v),
                    None => Err("pop on an empty queue".to_string()),
                }
            }
            "peek" => {
                want(name, args, 0)?;
                match self.items.front() {
                    Some(v) => Ok(v.clone()),
                    None => Err("peek on an empty queue".to_string()),
                }
            }
            "contains" => {
                want(name, args, 1)?;
                Ok(Value::Bool(
                    self.items.iter().any(|v| values_equal(v, &args[0])),
                ))
            }
            "to_list" => {
                want(name, args, 0)?;
                Ok(Value::List(new_ref(VecData {
                    items: self.items.iter().map(deep_clone).collect(),
                })))
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::Queue(new_ref(QueueData {
                    items: self.items.iter().map(deep_clone).collect(),
                })))
            }
            _ => Err(unknown(name, "queue")),
        }
    }
}

impl DequeData {
    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.items.len() as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.items.is_empty()))
            }
            "clear" => {
                want(name, args, 0)?;
                self.items.clear();
                Ok(Value::Null)
            }
            "push_back" | "push" => {
                want(name, args, 1)?;
                self.items.push_back(args[0].clone());
                Ok(Value::Null)
            }
            "push_front" => {
                want(name, args, 1)?;
                self.items.push_front(args[0].clone());
                Ok(Value::Null)
            }
            "pop_back" | "pop" => {
                want(name, args, 0)?;
                match self.items.pop_back() {
                    Some(v) => Ok(v),
                    None => Err("pop on an empty deque".to_string()),
                }
            }
            "pop_front" => {
                want(name, args, 0)?;
                match self.items.pop_front() {
                    Some(v) => Ok(v),
                    None => Err("pop_back/front on an empty deque".to_string()),
                }
            }
            "peek_back" | "peek" => {
                want(name, args, 0)?;
                match self.items.back() {
                    Some(v) => Ok(v.clone()),
                    None => Err("peek on an empty deque".to_string()),
                }
            }
            "peek_front" => {
                want(name, args, 0)?;
                match self.items.front() {
                    Some(v) => Ok(v.clone()),
                    None => Err("peek on an empty deque".to_string()),
                }
            }
            "contains" => {
                want(name, args, 1)?;
                Ok(Value::Bool(
                    self.items.iter().any(|v| values_equal(v, &args[0])),
                ))
            }
            "to_list" => {
                want(name, args, 0)?;
                Ok(Value::List(new_ref(VecData {
                    items: self.items.iter().map(deep_clone).collect(),
                })))
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::Deque(new_ref(DequeData {
                    items: self.items.iter().map(deep_clone).collect(),
                })))
            }
            _ => Err(unknown(name, "deque")),
        }
    }
}

// --------------------------------------------------------------------- map

/// Insertion-ordered map. Keys are null, bool, int, or string; floats are
/// rejected. Missing keys are errors, not silent nulls.
///
/// Storage is an insertion-ordered `Vec` (what `display`, iteration and
/// `keys`/`values`/`items` walk) plus a side `HashMap` index for O(1)
/// `find` — without it every `get`/`set`/`remove` was a linear scan and map
/// benchmarks degraded quadratically. The index is rebuilt on structural
/// removals (rare relative to lookups).
#[derive(Debug, Default)]
pub struct MapData {
    pub entries: Vec<(super::MapKey, Value)>,
    /// key -> position in `entries`. Kept in sync by `find`-adjacent
    /// mutations; rebuilt wholesale when entries shift.
    index: std::collections::HashMap<super::MapKey, usize>,
}

impl Drop for MapData {
    fn drop(&mut self) {
        // F4: same iterative teardown as VecData — a map whose values nest
        // maps/lists deeply must not recurse per level when the last owner
        // drops. Values from sole-owned child maps/lists are drained into a
        // worklist; keys (usually scalars) drop inline.
        let entries = std::mem::take(&mut self.entries);
        let mut work: Vec<Value> = Vec::with_capacity(entries.len());
        for (_, v) in entries {
            work.push(v);
        }
        while let Some(v) = work.pop() {
            let sole = match &v {
                Value::Map(arc) => std::sync::Arc::strong_count(arc) == 1,
                Value::List(arc) | Value::VecDyn(arc) => {
                    std::sync::Arc::strong_count(arc) == 1
                }
                _ => false,
            };
            if !sole {
                drop(v);
                continue;
            }
            match v {
                Value::Map(arc) => {
                    if let Ok(data) = std::sync::Arc::try_unwrap(arc) {
                        let mut data = data.into_inner().unwrap_or_else(|e| e.into_inner());
                        for (_, v) in data.entries.drain(..) {
                            work.push(v);
                        }
                    }
                }
                Value::List(arc) | Value::VecDyn(arc) => {
                    if let Ok(data) = std::sync::Arc::try_unwrap(arc) {
                        let mut data = data.into_inner().unwrap_or_else(|e| e.into_inner());
                        work.extend(std::mem::take(&mut data.items));
                    }
                }
                _ => drop(v),
            }
        }
    }
}

impl MapData {
    /// Build a map from entries (last duplicate wins, like the `map(...)`
    /// constructor). The only sanctioned way to construct `MapData`.
    pub fn from_entries(entries: Vec<(super::MapKey, Value)>) -> MapData {
        let mut index = std::collections::HashMap::with_capacity(entries.len());
        for (i, (k, _)) in entries.iter().enumerate() {
            index.insert(k.clone(), i);
        }
        MapData { entries, index }
    }

    #[inline]
    pub fn find(&self, k: &super::MapKey) -> Option<usize> {
        self.index.get(k).copied()
    }
    #[inline]
    pub fn set_key(&mut self, k: super::MapKey, val: Value) {
        match self.find(&k) {
            Some(i) => self.entries[i].1 = val,
            None => {
                self.index.insert(k.clone(), self.entries.len());
                self.entries.push((k, val));
            }
        }
    }
    /// Rebuild the side index after `entries` shifted (remove/clear paths).
    fn reindex(&mut self) {
        self.index.clear();
        for (i, (k, _)) in self.entries.iter().enumerate() {
            self.index.insert(k.clone(), i);
        }
    }
    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.entries.len() as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.entries.is_empty()))
            }
            "clear" => {
                want(name, args, 0)?;
                self.entries.clear();
                self.index.clear();
                Ok(Value::Null)
            }
            "set" => {
                want(name, args, 2)?;
                let k = as_key(&args[0])?;
                self.set_key(k, args[1].clone());
                Ok(Value::Null)
            }
            "get" => {
                want(name, args, 1)?;
                let k = as_key(&args[0])?;
                match self.find(&k) {
                    Some(i) => Ok(self.entries[i].1.clone()),
                    None => Err(format!("missing key {}", k.display())),
                }
            }
            "remove" => {
                want(name, args, 1)?;
                let k = as_key(&args[0])?;
                match self.find(&k) {
                    Some(i) => {
                        let v = self.entries.remove(i).1;
                        self.reindex();
                        Ok(v)
                    }
                    None => Err(format!("missing key {}", k.display())),
                }
            }
            "delete" => {
                want(name, args, 1)?;
                let k = as_key(&args[0])?;
                match self.find(&k) {
                    Some(i) => {
                        self.entries.remove(i);
                        self.reindex();
                        Ok(Value::Null)
                    }
                    None => Err(format!("missing key {}", k.display())),
                }
            }
            "has" => {
                want(name, args, 1)?;
                let k = as_key(&args[0])?;
                Ok(Value::Bool(self.find(&k).is_some()))
            }
            "items" => {
                want(name, args, 0)?;
                Ok(Value::List(new_ref(VecData {
                    items: self
                        .entries
                        .iter()
                        .map(|(k, v)| {
                            Value::Tuple(new_ref(vec![key_to_value(k), v.clone()]))
                        })
                        .collect(),
                })))
            }
            "contains" => {
                want(name, args, 1)?;
                let k = as_key(&args[0])?;
                Ok(Value::Bool(self.find(&k).is_some()))
            }
            "keys" => {
                want(name, args, 0)?;
                Ok(Value::List(new_ref(VecData {
                    items: self.entries.iter().map(|(k, _)| key_to_value(k)).collect(),
                })))
            }
            "values" => {
                want(name, args, 0)?;
                Ok(map_values_out(&self.entries))
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::Map(new_ref(MapData::from_entries(
                    self.entries
                        .iter()
                        .map(|(k, v)| (k.clone(), deep_clone(v)))
                        .collect(),
                ))))
            }
            _ => Err(unknown(name, "map")),
        }
    }
}

/// `map.values()` list built from an entries snapshot. Method dispatch used to
/// run this `deep_clone` loop while the map's mutex was held; a map containing
/// itself as a value re-locked it and hung (audit F3/p05). Callers snapshot
/// `entries` under the lock and build the result here without it.
pub fn map_values_out(entries: &[(super::MapKey, Value)]) -> Value {
    Value::List(new_ref(VecData {
        items: entries.iter().map(|(_, v)| deep_clone(v)).collect(),
    }))
}

// --------------------------------------------------------------------- set

/// Unique membership, insertion order kept for display and iteration.
#[derive(Debug, Default)]
pub struct SetData {
    pub items: Vec<Value>,
}

impl Drop for SetData {
    fn drop(&mut self) {
        // F4: iterative teardown, same contract as VecData above.
        let mut work = std::mem::take(&mut self.items);
        while let Some(v) = work.pop() {
            let sole = match &v {
                Value::Set(arc) => std::sync::Arc::strong_count(arc) == 1,
                Value::List(arc) | Value::VecDyn(arc) => {
                    std::sync::Arc::strong_count(arc) == 1
                }
                _ => false,
            };
            if !sole {
                drop(v);
                continue;
            }
            match v {
                Value::Set(arc) => {
                    if let Ok(data) = std::sync::Arc::try_unwrap(arc) {
                        let mut data = data.into_inner().unwrap_or_else(|e| e.into_inner());
                        work.extend(std::mem::take(&mut data.items));
                    }
                }
                Value::List(arc) | Value::VecDyn(arc) => {
                    if let Ok(data) = std::sync::Arc::try_unwrap(arc) {
                        let mut data = data.into_inner().unwrap_or_else(|e| e.into_inner());
                        work.extend(std::mem::take(&mut data.items));
                    }
                }
                _ => drop(v),
            }
        }
    }
}

impl SetData {
    fn find(&self, v: &Value) -> Option<usize> {
        self.items.iter().position(|x| values_equal(x, v))
    }
    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.items.len() as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.items.is_empty()))
            }
            "clear" => {
                want(name, args, 0)?;
                self.items.clear();
                Ok(Value::Null)
            }
            "push" | "add" => {
                want(name, args, 1)?;
                if self.find(&args[0]).is_none() {
                    self.items.push(args[0].clone());
                }
                Ok(Value::Null)
            }
            "remove" => {
                want(name, args, 1)?;
                match self.find(&args[0]) {
                    Some(i) => {
                        self.items.remove(i);
                        Ok(Value::Null)
                    }
                    None => Err("value not in set".to_string()),
                }
            }
            "contains" => {
                want(name, args, 1)?;
                Ok(Value::Bool(self.find(&args[0]).is_some()))
            }
            "union" => {
                want(name, args, 1)?;
                let other_items = as_set(&args[0])?.items.clone();
                set_op(name, self.items.clone(), other_items)
            }
            "intersection" | "intersect" => {
                want(name, args, 1)?;
                let other_items = as_set(&args[0])?.items.clone();
                set_op(name, self.items.clone(), other_items)
            }
            "difference" => {
                want(name, args, 1)?;
                let other_items = as_set(&args[0])?.items.clone();
                set_op(name, self.items.clone(), other_items)
            }
            "to_list" => {
                want(name, args, 0)?;
                Ok(Value::List(new_ref(VecData {
                    items: self.items.iter().map(deep_clone).collect(),
                })))
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::Set(new_ref(SetData {
                    items: self.items.iter().map(deep_clone).collect(),
                })))
            }
            _ => Err(unknown(name, "set")),
        }
    }
}

/// Set algebra over item snapshots. The old `union`/`intersection`/
/// `difference` arms ran `values_equal` while holding both the receiver's
/// mutex (method dispatch) and the argument's (`as_set` guard); with sets that
/// reference each other the comparison re-locked both and hung forever
/// (audit F3/p03). Callers clone `items` under the locks and run this helper
/// with no lock held, so `values_equal`/`deep_clone` may re-lock freely.
pub fn set_op(
    op: &str,
    self_items: Vec<Value>,
    other_items: Vec<Value>,
) -> Result<Value, String> {
    match op {
        "union" => {
            let mut items = self_items;
            for v in other_items.iter() {
                if !items.iter().any(|x| values_equal(x, v)) {
                    items.push(deep_clone(v));
                }
            }
            Ok(Value::Set(new_ref(SetData { items })))
        }
        "intersection" | "intersect" => {
            let items: Vec<Value> = self_items
                .iter()
                .filter(|v| other_items.iter().any(|w| values_equal(v, w)))
                .map(deep_clone)
                .collect();
            Ok(Value::Set(new_ref(SetData { items })))
        }
        "difference" => {
            let items: Vec<Value> = self_items
                .iter()
                .filter(|v| !other_items.iter().any(|w| values_equal(v, w)))
                .map(deep_clone)
                .collect();
            Ok(Value::Set(new_ref(SetData { items })))
        }
        _ => Err(unknown(op, "set")),
    }
}

fn as_set(v: &Value) -> Result<std::sync::MutexGuard<'_, SetData>, String> {
    match v {
        Value::Set(d) => Ok(d.lock().unwrap()),
        other => Err(format!("expected a set, found {}", super::type_name(other))),
    }
}

pub fn as_key(v: &Value) -> Result<super::MapKey, String> {
    super::as_map_key(v)
}

// ------------------------------------------------------------------ helpers

pub fn want(name: &str, args: &[Value], n: usize) -> Result<(), String> {
    if args.len() == n {
        Ok(())
    } else {
        Err(format!(
            "{} expects {} argument(s), got {}",
            name,
            n,
            args.len()
        ))
    }
}

pub fn unknown(name: &str, ty: &str) -> String {
    format!("unknown method '{}' on {}", name, ty)
}

pub fn want_index(v: &Value, who: &str) -> Result<usize, String> {
    match v {
        Value::Int(i) if *i >= 0 => Ok(*i as usize),
        other => Err(format!(
            "{} expects a non-negative int index, found {}",
            who,
            super::type_name(other)
        )),
    }
}

pub fn want_num(v: &Value, who: &str) -> Result<f64, String> {
    match v {
        Value::Int(i) => Ok(*i as f64),
        Value::Float(f) => Ok(*f),
        other => Err(format!(
            "{} expects a number, found {}",
            who,
            super::type_name(other)
        )),
    }
}

pub fn want_int(v: &Value, who: &str) -> Result<i64, String> {
    match v {
        Value::Int(i) => Ok(*i),
        other => Err(format!(
            "{} expects an int, found {}",
            who,
            super::type_name(other)
        )),
    }
}

pub fn want_str(v: &Value, who: &str) -> Result<String, String> {
    match v {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(format!(
            "{} expects a string, found {}",
            who,
            super::type_name(other)
        )),
    }
}

/// Total order for numbers and strings. Booleans and nulls are not ordered
/// against other types; sorting or tree insertion of mixed values errors.
pub fn value_cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Some(x.cmp(y)),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

pub fn heap_less(a: &Value, b: &Value) -> Result<bool, String> {
    match value_cmp(a, b) {
        Some(o) => Ok(o == std::cmp::Ordering::Less),
        None => Err("values are not comparable".to_string()),
    }
}

/// Interpret what a user comparator (`sort_by`'s argument) returned.
///
/// A comparator may answer either a `bool` (`true` means "`a` is less than
/// `b`", so `false` means greater-or-equal and is normalized to `Greater`) or
/// a number (negative → less, `0` → equal, positive → greater). A NaN has no
/// order at all, so it is reported rather than silently folded into `Equal`.
pub fn comparator_ordering(
    meth: &str,
    v: &Value,
) -> Result<std::cmp::Ordering, String> {
    match v {
        Value::Bool(true) => Ok(std::cmp::Ordering::Less),
        Value::Bool(false) => Ok(std::cmp::Ordering::Greater),
        Value::Int(i) => Ok(i.cmp(&0)),
        Value::Float(f) => f.partial_cmp(&0.0).ok_or_else(|| {
            format!("{}: comparator returned NaN, which has no order", meth)
        }),
        other => Err(format!(
            "{}: comparator must return a bool, int, or float, found {}",
            meth,
            super::type_name(other)
        )),
    }
}

/// Stable bottom-up merge sort over `order`, a permutation of the indices
/// into the caller's snapshot. `order` and `scratch` must both be `n` long.
///
/// This is deliberately hand-written: `slice::sort_by` is allowed to panic
/// when the comparator does not implement a total order, and a comparator
/// written in Spid3r can be anything (`fn(a, b) { return a > b; }` is not
/// transitive-safe in combination with `b > a`). A program must get a
/// deterministic result or a clean error — never a host panic (Invariant #3).
///
/// Taking the left run on `Equal` is what makes the sort stable.
pub fn merge_sort_indices<F>(
    order: &mut [usize],
    scratch: &mut [usize],
    cmp: &mut F,
) -> Result<(), String>
where
    F: FnMut(usize, usize) -> Result<std::cmp::Ordering, String>,
{
    let n = order.len();
    let mut width = 1;
    while width < n {
        let mut i = 0;
        while i < n {
            let mid = (i + width).min(n);
            let end = (i + 2 * width).min(n);
            if mid < end {
                let (mut a, mut b) = (i, mid);
                for slot in scratch[i..end].iter_mut() {
                    if a >= mid {
                        // Left run exhausted: the right run is already in order.
                        *slot = order[b];
                        b += 1;
                    } else if b >= end {
                        *slot = order[a];
                        a += 1;
                    } else if cmp(order[a], order[b])? == std::cmp::Ordering::Greater {
                        *slot = order[b];
                        b += 1;
                    } else {
                        *slot = order[a];
                        a += 1;
                    }
                }
            } else {
                // Nothing to merge: carry the lone run through untouched.
                scratch[i..end].copy_from_slice(&order[i..end]);
            }
            i = end;
        }
        order.copy_from_slice(scratch);
        width = width.saturating_mul(2);
    }
    Ok(())
}

/// Reorder `items` by `order` (a permutation of `0..items.len()`).
pub fn permuted(items: &[Value], order: &[usize]) -> Vec<Value> {
    order.iter().map(|&i| items[i].clone()).collect()
}

/// The index permutation that would sort `items` into natural ascending
/// order: the numbers-or-strings rule `list.sort()` enforces, applied to
/// positions instead of values. `argsort()[i]` is the position in the
/// original list of the element that belongs at position `i`.
pub fn argsort_indices(items: &[Value]) -> Result<Vec<usize>, String> {
    let all_num = items
        .iter()
        .all(|v| matches!(v, Value::Int(_) | Value::Float(_)));
    let all_str = items.iter().all(|v| matches!(v, Value::Str(_)));
    if !(all_num || all_str) {
        return Err("cannot sort: values are not all numbers or all strings".to_string());
    }
    let mut order: Vec<usize> = (0..items.len()).collect();
    let mut scratch = vec![0usize; items.len()];
    // Unreachable in practice: the pre-check above guarantees comparability.
    merge_sort_indices(&mut order, &mut scratch, &mut |i, j| {
        Ok(value_cmp(&items[i], &items[j]).unwrap_or(std::cmp::Ordering::Equal))
    })?;
    Ok(order)
}

/// Shared: lock a `VecData`-shaped value (list or vec) for read-only use.
pub fn vec_items(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::List(d) | Value::VecDyn(d) => {
            // Snapshot under the lock, deep-clone outside it (audit F3: items
            // can reach the container itself and deep_clone re-locks it).
            let items = d.lock().unwrap().items.clone();
            Ok(items.iter().map(deep_clone).collect())
        }
        other => Err(format!(
            "expected a list, found {}",
            super::type_name(other)
        )),
    }
}

pub fn make_list(items: Vec<Value>) -> Value {
    Value::List(new_ref(VecData { items }))
}
