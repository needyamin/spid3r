//! Sequence collections: list, vec, stack, queue, deque, map, set.
//! All of these are real structures with the documented semantics; a list is
//! an indexed vector, a stack is strict LIFO on a vector, a queue is strict
//! FIFO on a ring-buffer deque.

use std::collections::VecDeque;

use super::{deep_clone, key_to_value, new_ref, values_equal, Value};

// ---------------------------------------------------------------- list, vec

#[inline]
pub fn is_non_recursive_scalar(v: &Value) -> bool {
    matches!(
        v,
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
    )
}

#[derive(Debug)]
pub struct VecData {
    items: Vec<Value>,
    gen: u64,
    dense_i64: Option<Vec<i64>>,
    dense_f64: Option<Vec<f64>>,
    pub gc_tracked: bool,
}

impl Default for VecData {
    fn default() -> Self {
        Self::from_items(Vec::new())
    }
}

impl Drop for VecData {
    fn drop(&mut self) {
        if self.items.iter().all(is_non_recursive_scalar) {
            return;
        }
        self.dense_i64 = None;
        self.dense_f64 = None;
        // F4/A-17: drain nested containers iteratively; the shared helper knows
        // every container type, so mixed chains (list -> stack -> map -> ...)
        // are covered too.
        super::drop_iteratively(std::mem::take(&mut self.items));
    }
}

impl VecData {
    pub fn from_items(items: Vec<Value>) -> Self {
        Self {
            items,
            gen: 0,
            dense_i64: None,
            dense_f64: None,
            gc_tracked: true,
        }
    }

    #[inline]
    pub fn from_items_untracked(items: Vec<Value>) -> Self {
        Self {
            items,
            gen: 0,
            dense_i64: None,
            dense_f64: None,
            gc_tracked: false,
        }
    }

    #[inline]
    pub fn needs_track_for(&mut self, v: &Value) -> bool {
        if !self.gc_tracked && !is_non_recursive_scalar(v) {
            self.gc_tracked = true;
            true
        } else {
            false
        }
    }

    #[inline]
    pub fn as_slice(&self) -> &[Value] {
        &self.items
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[inline]
    pub fn generation(&self) -> u64 {
        self.gen
    }

    #[inline]
    pub fn clone_items(&self) -> Vec<Value> {
        self.items.clone()
    }

    #[inline]
    pub fn invalidate(&mut self) {
        self.gen = self.gen.wrapping_add(1);
        self.dense_i64 = None;
        self.dense_f64 = None;
    }

    #[inline]
    pub fn push_untracked(&mut self, v: Value) {
        self.items.push(v);
    }

    #[inline]
    pub fn push_item(&mut self, v: Value) {
        self.gen = self.gen.wrapping_add(1);
        if let Some(ref mut d) = self.dense_i64 {
            if let Value::Int(n) = v {
                d.push(n);
            } else {
                self.dense_i64 = None;
            }
        }
        if let Some(ref mut d) = self.dense_f64 {
            if let Value::Float(f) = v {
                d.push(f);
            } else {
                self.dense_f64 = None;
            }
        }
        self.items.push(v);
    }

    #[inline]
    pub fn pop_item(&mut self) -> Option<Value> {
        self.gen = self.gen.wrapping_add(1);
        if let Some(ref mut d) = self.dense_i64 {
            d.pop();
        }
        if let Some(ref mut d) = self.dense_f64 {
            d.pop();
        }
        self.items.pop()
    }

    #[inline]
    pub fn set_item(&mut self, idx: usize, v: Value) {
        self.gen = self.gen.wrapping_add(1);
        if let Some(ref mut d) = self.dense_i64 {
            if let Value::Int(n) = v {
                if let Some(slot) = d.get_mut(idx) {
                    *slot = n;
                } else {
                    self.dense_i64 = None;
                }
            } else {
                self.dense_i64 = None;
            }
        }
        if let Some(ref mut d) = self.dense_f64 {
            if let Value::Float(f) = v {
                if let Some(slot) = d.get_mut(idx) {
                    *slot = f;
                } else {
                    self.dense_f64 = None;
                }
            } else {
                self.dense_f64 = None;
            }
        }
        self.items[idx] = v;
    }

    #[inline]
    pub fn insert_item(&mut self, idx: usize, v: Value) {
        self.invalidate();
        self.items.insert(idx, v);
    }

    #[inline]
    pub fn remove_item(&mut self, idx: usize) -> Value {
        self.invalidate();
        self.items.remove(idx)
    }

    #[inline]
    pub fn clear_items(&mut self) {
        self.invalidate();
        self.items.clear();
    }

    #[inline]
    pub fn extend_items(&mut self, extra: impl IntoIterator<Item = Value>) {
        self.invalidate();
        self.items.extend(extra);
    }

    #[inline]
    pub fn set_items(&mut self, items: Vec<Value>) {
        self.invalidate();
        self.items = items;
    }

    #[inline]
    pub fn take_items(&mut self) -> Vec<Value> {
        self.invalidate();
        std::mem::take(&mut self.items)
    }

    pub(crate) fn clear_for_pool(&mut self, out: &mut Vec<Value>) -> bool {
        if self.gc_tracked || self.items.capacity() > 4096 {
            return false;
        }
        if self.items.iter().all(is_non_recursive_scalar) {
            self.items.clear();
        } else {
            for v in self.items.drain(..) {
                if !is_non_recursive_scalar(&v) {
                    out.push(v);
                }
            }
        }
        self.gen = 0;
        self.dense_i64 = None;
        self.dense_f64 = None;
        true
    }

    /// Ensure an unboxed dense `i64` mirror of `items` exists when every
    /// element is `Value::Int`, returning `(ptr, len, gen)`.
    pub fn ensure_dense_i64(&mut self) -> Option<(*const i64, usize, u64)> {
        if self.dense_i64.is_none() {
            let mut buf = Vec::with_capacity(self.items.len());
            for it in &self.items {
                if let Value::Int(n) = *it {
                    buf.push(n);
                } else {
                    return None;
                }
            }
            self.dense_f64 = None;
            self.dense_i64 = Some(buf);
        }
        self.dense_i64
            .as_ref()
            .map(|d| (d.as_ptr(), d.len(), self.gen))
    }

    /// Ensure an unboxed dense `f64` mirror of `items` exists when every
    /// element is `Value::Float`, returning `(ptr, len, gen)`.
    pub fn ensure_dense_f64(&mut self) -> Option<(*const f64, usize, u64)> {
        if self.dense_f64.is_none() {
            let mut buf = Vec::with_capacity(self.items.len());
            for it in &self.items {
                if let Value::Float(f) = *it {
                    buf.push(f);
                } else {
                    return None;
                }
            }
            self.dense_i64 = None;
            self.dense_f64 = Some(buf);
        }
        self.dense_f64
            .as_ref()
            .map(|d| (d.as_ptr(), d.len(), self.gen))
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
                self.clear_items();
                Ok(Value::Null)
            }
            "capacity" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.items.capacity() as i64))
            }
            "push" => {
                want(name, args, 1)?;
                self.push_item(args[0].clone());
                Ok(Value::Null)
            }
            "pop" => {
                want(name, args, 0)?;
                match self.pop_item() {
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
                if i < self.items.len() {
                    self.set_item(i, args[1].clone());
                    Ok(Value::Null)
                } else {
                    Err(format!(
                        "index {} out of bounds for length {}",
                        i,
                        self.items.len()
                    ))
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
                self.insert_item(i, args[1].clone());
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
                Ok(self.remove_item(i))
            }
            "remove" => {
                want(name, args, 1)?;
                let pos = self.items.iter().position(|v| values_equal(v, &args[0]));
                match pos {
                    Some(i) => Ok(self.remove_item(i)),
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
                self.invalidate();
                self.items.reverse();
                Ok(Value::Null)
            }
            "sort" => {
                want(name, args, 0)?;
                let mut vals = self.take_items();
                let all_num = vals
                    .iter()
                    .all(|v| matches!(v, Value::Int(_) | Value::Float(_)));
                let all_str = vals.iter().all(|v| matches!(v, Value::Str(_)));
                if !(all_num || all_str) {
                    self.set_items(vals);
                    return Err(
                        "cannot sort: values are not all numbers or all strings".to_string()
                    );
                }
                vals.sort_by(natural_cmp);
                self.set_items(vals);
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
                Ok(Value::List(new_ref(VecData::from_items(
                    self.items[a..b].iter().map(deep_clone).collect(),
                ))))
            }
            "join" => {
                want(name, args, 1)?;
                let sep = want_str(&args[0], "list.join")?;
                // F-6: non-string elements are auto-stringified with the same
                // canonical representation as `print`/`string()`/interpolation
                // (display_value), so `[1, 2].join("-")` is "1-2". All-string
                // lists are byte-for-byte what they always were.
                Ok(Value::Str(join_parts(&self.items, &sep).into()))
            }
            "append" => {
                want(name, args, 1)?;
                let other = match &args[0] {
                    Value::List(d) | Value::VecDyn(d) => {
                        let d = d.lock().unwrap();
                        d.clone_items()
                    }
                    other => {
                        return Err(format!(
                            "append expects a list, found {}",
                            super::type_name(other)
                        ))
                    }
                };
                self.extend_items(other);
                Ok(Value::Null)
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::List(new_ref(VecData::from_items(
                    self.items.iter().map(deep_clone).collect(),
                ))))
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
                vals.sort_by(natural_cmp);
                Ok(Value::List(new_ref(VecData::from_items(vals))))
            }
            "reversed" => {
                want(name, args, 0)?;
                let mut vals = self.items.clone();
                vals.reverse();
                Ok(Value::List(new_ref(VecData::from_items(vals))))
            }
            _ => Err(unknown(name, "list")),
        }
    }
}

// ------------------------------------------------- detached list operations

/// Marker returned by a detached mutator when the container changed between the
/// snapshot it resolved against and the commit. The dispatcher retries with a
/// fresh snapshot (bounded); this never reaches user code.
pub const CONFLICT: &str = "spid3r-internal: container changed during commit";

/// Did `cur` keep exactly the elements `snapshot` had, in the same slots?
/// Identity/scalar comparison only — never locks a value, so it is safe to run
/// while holding the container's mutex (see [`crate::value::same_slot`]).
pub fn slots_unchanged(cur: &[Value], snapshot: &[Value]) -> bool {
    cur.len() == snapshot.len()
        && cur
            .iter()
            .zip(snapshot)
            .all(|(a, b)| crate::value::same_slot(a, b))
}

/// Detached list/vec operations (audit A-7): `contains`/`index_of`/`remove`/
/// `slice` compare or deep-clone item values, and an item can be (or reach)
/// the receiver itself — running them under the receiver's Mutex would
/// re-lock it and hang. The dispatcher snapshots `items` with the lock
/// released before calling here; `d` is only re-locked for the tiny
/// structural commit of `remove` (no value traversal under the lock).
pub fn vec_detached(
    d: &crate::value::Ref<VecData>,
    items: &[Value],
    name: &str,
    args: &[Value],
) -> Result<Value, String> {
    match name {
        "contains" => {
            want(name, args, 1)?;
            Ok(Value::Bool(items.iter().any(|v| values_equal(v, &args[0]))))
        }
        "index_of" => {
            want(name, args, 1)?;
            let pos = items.iter().position(|v| values_equal(v, &args[0]));
            Ok(Value::Int(pos.map(|p| p as i64).unwrap_or(-1)))
        }
        "remove" => {
            want(name, args, 1)?;
            let pos = items.iter().position(|v| values_equal(v, &args[0]));
            match pos {
                Some(i) => {
                    let mut g = d.lock().unwrap();
                    // Confirm the slot still holds the element the snapshot
                    // matched (identity/scalar check — no locking), so a
                    // concurrent mutation cannot make us remove a different
                    // element. If it moved, the dispatcher retries.
                    match g.as_slice().get(i) {
                        Some(cur) if crate::value::same_slot(cur, &items[i]) => {
                            Ok(g.remove_item(i))
                        }
                        _ => Err(CONFLICT.to_string()),
                    }
                }
                None => Err("value not in list".to_string()),
            }
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
                items.len()
            };
            if a > b || b > items.len() {
                return Err(format!("slice range {}..{} out of bounds", a, b));
            }
            Ok(Value::List(new_ref(VecData::from_items(
                items[a..b].iter().map(deep_clone).collect(),
            ))))
        }
        "join" => {
            want(name, args, 1)?;
            let sep = want_str(&args[0], "list.join")?;
            // F-6: auto-stringify via the shared `join_parts` helper. join runs
            // detached from the list's lock because an element may be (or
            // reach) the receiver and `display_value` takes container locks of
            // its own (audit A-7 rationale).
            Ok(Value::Str(join_parts(items, &sep).into()))
        }
        _ => unreachable!(),
    }
}

/// F-6: join a list's elements with `sep`. Strings contribute their raw text;
/// every other value contributes its canonical display form (the same text
/// `print`, `string()` and interpolation produce), so `[1, 2].join("-")` is
/// `"1-2"` and `[true, null].join(",")` is `"true,null"`.
fn join_parts(items: &[Value], sep: &str) -> String {
    items
        .iter()
        .map(|v| match v {
            Value::Str(s) => s.to_string(),
            other => crate::value::display_value(other),
        })
        .collect::<Vec<_>>()
        .join(sep)
}

/// Detached `contains`/`to_list` shared by stack, queue, deque, and ring
/// buffer (audit A-7, same rationale as [`vec_detached`]).
pub fn seq_detached(items: &[Value], name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "contains" => {
            want(name, args, 1)?;
            Ok(Value::Bool(items.iter().any(|v| values_equal(v, &args[0]))))
        }
        "to_list" => {
            want(name, args, 0)?;
            Ok(items_to_list_detached(items))
        }
        _ => unreachable!(),
    }
}

/// Deep-clone a value snapshot into a plain list with no container lock held
/// (audit A-7): an item can be (or reach) the source container.
pub fn items_to_list_detached(items: &[Value]) -> Value {
    Value::List(new_ref(VecData::from_items(
        items.iter().map(deep_clone).collect(),
    )))
}

/// Detached set operations (audit A-7, same rationale as [`vec_detached`]).
pub fn set_detached(
    d: &crate::value::Ref<SetData>,
    items: Vec<Value>,
    name: &str,
    args: &[Value],
) -> Result<Value, String> {
    match name {
        "push" | "add" => {
            want(name, args, 1)?;
            if items.iter().any(|x| values_equal(x, &args[0])) {
                return Ok(Value::Null);
            }
            let mut g = d.lock().unwrap();
            // Verified commit (compare-and-swap): the snapshot said "not
            // present", so re-check that the set still matches the snapshot
            // before inserting — otherwise two tasks adding the same value
            // concurrently could both see "absent" and push duplicates.
            if !slots_unchanged(&g.items, &items) {
                return Err(CONFLICT.to_string());
            }
            g.push_unchecked(args[0].clone());
            Ok(Value::Null)
        }
        "remove" => {
            want(name, args, 1)?;
            match items.iter().position(|x| values_equal(x, &args[0])) {
                Some(i) => {
                    let mut g = d.lock().unwrap();
                    // Same-slot re-check as `vec_detached` (no locking): if a
                    // concurrent mutation shifted the slot, retry on a fresh
                    // snapshot instead of removing a different element.
                    match g.items.get(i) {
                        Some(cur) if crate::value::same_slot(cur, &items[i]) => {
                            g.remove_at(i);
                            Ok(Value::Null)
                        }
                        _ => Err(CONFLICT.to_string()),
                    }
                }
                None => Err("value not in set".to_string()),
            }
        }
        "contains" => {
            want(name, args, 1)?;
            Ok(Value::Bool(items.iter().any(|x| values_equal(x, &args[0]))))
        }
        "to_list" => {
            want(name, args, 0)?;
            Ok(items_to_list_detached(&items))
        }
        _ => unreachable!(),
    }
}

// ------------------------------------------------------------------- stack

#[derive(Debug, Default)]
pub struct StackData {
    /// Bottom first, top last.
    pub items: Vec<Value>,
}

impl Drop for StackData {
    fn drop(&mut self) {
        // A-17: a 200k-deep `stack` chain used to abort the process here.
        super::drop_iteratively(std::mem::take(&mut self.items));
    }
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
                Ok(Value::List(new_ref(VecData::from_items(
                    self.items.iter().map(deep_clone).collect(),
                ))))
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

impl Drop for QueueData {
    fn drop(&mut self) {
        // A-17: `VecDeque` has no `take`, so drain into a `Vec` first.
        super::drop_iteratively(self.items.drain(..).collect());
    }
}

impl Drop for DequeData {
    fn drop(&mut self) {
        super::drop_iteratively(self.items.drain(..).collect());
    }
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
                Ok(Value::List(new_ref(VecData::from_items(
                    self.items.iter().map(deep_clone).collect(),
                ))))
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
                Ok(Value::List(new_ref(VecData::from_items(
                    self.items.iter().map(deep_clone).collect(),
                ))))
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

/// Inline tag capacity for small maps (`entries.len() <= SMALL_MAP_MAX`) before
/// lazily allocating the open-addressed `slots` table (Track 3 RT-3).
const SMALL_MAP_MAX: usize = 16;

#[inline]
fn map_tag(hash: u64) -> u8 {
    (hash >> 56) as u8
}

/// Contiguous open-addressed cache of short-key (`<= 15`-byte) integer-valued
/// `MapData` entries matching `ValueV2`'s SSO representation for fast AOT reads.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DenseSsoI64Slot {
    pub tag: u64,
    pub payload: u64,
    pub val: i64,
}

/// Insertion-ordered map. Keys are null, bool, int, or string; floats are
/// rejected. Missing keys are errors, not silent nulls.
///
/// Storage is an insertion-ordered `Vec` (what `display`, iteration and
/// `keys`/`values`/`items` walk) plus a lightweight index for O(1) `find` and
/// zero-allocation `find_str`:
/// - For `entries.len() <= 16` (`slots.is_empty()`), a 16-byte inline hash-tag
///   array avoids any heap allocation (huge win for small JSON objects).
/// - Above 16 entries, a flat open-addressed `(hash, entry_idx + 1)` table
///   (`0` = empty) stores only indices without cloning `MapKey`s.
#[derive(Debug)]
pub struct MapData {
    pub entries: Vec<(super::MapKey, Value)>,
    /// 1-byte hash tags used when `slots.is_empty()` (`entries.len() <= 16`).
    small_tags: [u8; SMALL_MAP_MAX],
    /// Open-addressed linear-probing index `(hash >> 32, entry_idx + 1)` (`0` = empty).
    /// Capacity is always 0 or a power of two (8 bytes per slot).
    slots: Vec<(u32, u32)>,
    /// Lazily populated SSO+i64 open-addressed mirror invalidated by `bump_generation`.
    dense_sso_i64: Option<Vec<DenseSsoI64Slot>>,
    /// True when this map is a **module namespace** — the value bound to an
    /// import alias (`import "m" as mu`). Such a map is a read-only view of
    /// the module's exports: it cannot be mutated, and an exported name
    /// shadows a built-in map method (`mu.get()` is the module's `get`, not
    /// `map.get`).
    pub namespace: bool,
    pub gc_tracked: bool,
    /// The module's `public const` exports. They are frozen: a namespace copy
    /// of a constant must not be rebindable (`mu["VERSION"] = 9`).
    pub frozen: Option<Box<super::FxHashSet<super::MapKey>>>,
    /// Monotonic mutation counter (Phase 5). Bumped by every value or
    /// structural mutation so a future inline cache can detect invalidation.
    /// `Value::Map` is `Ref<MapData>` (pointer-sized `Arc<Mutex<..>>`), so
    /// adding this scalar does **not** change the `Value` layout and requires
    /// no GC change. Wrapping (not `+= 1`) keeps `overflow-checks = true`
    /// builds safe after 2^64 mutations.
    pub generation: u64,
}

impl Default for MapData {
    fn default() -> Self {
        Self::from_entries(Vec::new())
    }
}

impl Drop for MapData {
    fn drop(&mut self) {
        self.dense_sso_i64 = None;
        if self.entries.iter().all(|(_, v)| is_non_recursive_scalar(v)) {
            return;
        }
        // F4/A-17: iterative teardown through the shared helper. Only values are
        // walked; keys (`MapKey`) are hashable scalars and drop inline.
        let entries = std::mem::take(&mut self.entries);
        super::drop_iteratively(entries.into_iter().map(|(_, v)| v).collect());
    }
}

impl MapData {
    /// Build a map from entries (last duplicate wins, like the `map(...)`
    /// constructor). The only sanctioned way to construct `MapData`.
    pub fn from_entries(entries: Vec<(super::MapKey, Value)>) -> MapData {
        let mut m = MapData {
            entries,
            small_tags: [0; SMALL_MAP_MAX],
            slots: Vec::new(),
            dense_sso_i64: None,
            namespace: false,
            gc_tracked: true,
            frozen: None,
            generation: 0,
        };
        if m.entries.len() <= SMALL_MAP_MAX {
            for (i, (k, _)) in m.entries.iter().enumerate() {
                m.small_tags[i] = map_tag(super::fx_hash_map_key(k));
            }
        } else {
            m.rebuild_slots();
        }
        m
    }

    /// Build a map outside the cycle-GC registry (`gc_tracked: false`) when the
    /// constructor knows its entries contain no cycles (e.g. `json_parse`).
    #[inline]
    pub fn from_entries_untracked(
        entries: Vec<(super::MapKey, Value)>,
        small_tags: [u8; SMALL_MAP_MAX],
    ) -> MapData {
        let mut m = MapData {
            entries,
            small_tags,
            slots: Vec::new(),
            dense_sso_i64: None,
            namespace: false,
            gc_tracked: false,
            frozen: None,
            generation: 0,
        };
        if m.entries.len() > SMALL_MAP_MAX {
            m.rebuild_slots();
        }
        m
    }

    /// Build a small map (`entries.len() <= SMALL_MAP_MAX`) using pre-computed
    /// 1-byte hash tags (Track 3 RT-3 / RT-4).
    #[inline]
    pub fn from_entries_with_tags(
        entries: Vec<(super::MapKey, Value)>,
        small_tags: [u8; SMALL_MAP_MAX],
    ) -> MapData {
        debug_assert!(entries.len() <= SMALL_MAP_MAX);
        MapData {
            entries,
            small_tags,
            slots: Vec::new(),
            dense_sso_i64: None,
            namespace: false,
            gc_tracked: true,
            frozen: None,
            generation: 0,
        }
    }

    #[inline]
    pub fn needs_track_for(&mut self, v: &Value) -> bool {
        if !self.gc_tracked && !is_non_recursive_scalar(v) {
            self.gc_tracked = true;
            true
        } else {
            false
        }
    }

    /// Insert or update a parsed string-keyed entry while populating a freshly checked-out map.
    #[inline]
    pub fn set_parsed_str_entry(&mut self, key: super::KeyStr, tag: u8, value: Value) {
        let len = self.entries.len();
        if len <= SMALL_MAP_MAX {
            for i in 0..len {
                if self.small_tags[i] == tag {
                    if let super::MapKey::Str(existing) = &self.entries[i].0 {
                        if existing == &key {
                            self.entries[i].1 = value;
                            return;
                        }
                    }
                }
            }
            if len < SMALL_MAP_MAX {
                self.small_tags[len] = tag;
            }
            self.entries.push((super::MapKey::Str(key), value));
            return;
        }
        let mk = super::MapKey::Str(key);
        if let Some(pos) = self.entries.iter().position(|(k, _)| k == &mk) {
            self.entries[pos].1 = value;
        } else {
            self.entries.push((mk, value));
        }
    }

    /// Fast path for `set_parsed_str_entry` using a per-object 64-bit `(tag & 63)` bloom mask
    /// to skip scanning `small_tags` when keys have distinct tag bits.
    #[inline]
    pub fn set_parsed_str_entry_masked(
        &mut self,
        key: super::KeyStr,
        tag: u8,
        value: Value,
        seen_mask: &mut u64,
    ) {
        let len = self.entries.len();
        let bit = 1u64 << (tag & 63);
        if len < SMALL_MAP_MAX && (*seen_mask & bit) == 0 {
            *seen_mask |= bit;
            self.small_tags[len] = tag;
            self.entries.push((super::MapKey::Str(key), value));
            return;
        }
        self.set_parsed_str_entry(key, tag, value);
    }

    /// Finalize open-addressed index when a parsed map exceeded `SMALL_MAP_MAX`.
    #[inline]
    pub fn finalize_parsed(&mut self) {
        self.dense_sso_i64 = None;
        if self.entries.len() > SMALL_MAP_MAX {
            self.rebuild_slots();
        }
    }

    /// Rebuild the open-addressed `slots` table from `self.entries`.
    fn rebuild_slots(&mut self) {
        let cap = (self.entries.len().saturating_mul(2))
            .next_power_of_two()
            .max(32);
        let mut slots = vec![(0u32, 0u32); cap];
        let mask = cap - 1;
        for (i, (k, _)) in self.entries.iter().enumerate() {
            let h = super::fx_hash_map_key(k);
            let h32 = (h >> 32) as u32;
            let mut pos = (h as usize) & mask;
            loop {
                let (sh, idx1) = slots[pos];
                if idx1 == 0 {
                    slots[pos] = (h32, (i + 1) as u32);
                    break;
                }
                if sh == h32 && self.entries[(idx1 - 1) as usize].0 == *k {
                    // Last duplicate wins, matching previous FxHashMap::insert.
                    slots[pos].1 = (i + 1) as u32;
                    break;
                }
                pos = (pos + 1) & mask;
            }
        }
        self.slots = slots;
    }

    /// Invalidate any cached resolution for this map. Called from every
    /// mutation path (`set_key`, `remove_at`, `clear`).
    #[inline]
    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.dense_sso_i64 = None;
    }

    /// Build or return an open-addressed table for `<= 15`-byte string keys -> `i64`
    /// values, returning `(slots_ptr, mask)`. Returns `None` if any entry has a
    /// non-string key, key length `> 15`, or non-int value.
    pub fn ensure_dense_sso_i64(&mut self) -> Option<(*const DenseSsoI64Slot, usize)> {
        if self.dense_sso_i64.is_none() {
            for (k, v) in &self.entries {
                let super::MapKey::Str(ks) = k else {
                    return None;
                };
                if ks.as_bytes().len() > 15 || !matches!(v, Value::Int(_)) {
                    return None;
                }
            }
            let cap = (self.entries.len().saturating_mul(2))
                .next_power_of_two()
                .max(8);
            let mask = cap - 1;
            let mut slots = vec![
                DenseSsoI64Slot {
                    tag: 0,
                    payload: 0,
                    val: 0,
                };
                cap
            ];
            for (k, v) in &self.entries {
                let (super::MapKey::Str(ks), Value::Int(n)) = (k, v) else {
                    unreachable!("validated all entries above");
                };
                let v2 = crate::value::ValueV2::from_sso_bytes(ks.as_bytes());
                let h = (v2.tag ^ v2.payload).wrapping_mul(0x517c_c1b7_2722_0a95);
                let mut pos = (((h >> 32) ^ h) as usize) & mask;
                loop {
                    let s = &mut slots[pos];
                    if s.tag == 0 || (s.tag == v2.tag && s.payload == v2.payload) {
                        *s = DenseSsoI64Slot {
                            tag: v2.tag,
                            payload: v2.payload,
                            val: *n,
                        };
                        break;
                    }
                    pos = (pos + 1) & mask;
                }
            }
            self.dense_sso_i64 = Some(slots);
        }
        self.dense_sso_i64
            .as_ref()
            .map(|v| (v.as_ptr(), v.len() - 1))
    }

    /// Build a **module namespace**: the exported names of an imported module.
    /// `frozen` holds the module's `public const` exports, which stay
    /// immutable here exactly as they are in the module itself.
    pub fn namespace(
        entries: Vec<(super::MapKey, Value)>,
        frozen: super::FxHashSet<super::MapKey>,
    ) -> MapData {
        let mut m = MapData::from_entries(entries);
        m.namespace = true;
        m.frozen = if frozen.is_empty() {
            None
        } else {
            Some(Box::new(frozen))
        };
        m
    }

    /// Whether a key may not be rebound (a `public const` export).
    #[inline]
    pub fn is_frozen(&self, k: &super::MapKey) -> bool {
        self.frozen.as_ref().is_some_and(|f| f.contains(k))
    }

    /// The error a mutation of this map must produce, or `None` when the
    /// mutation is allowed. A namespace is a read-only view of a module's
    /// exports; its constants are frozen even in principle.
    pub fn mutation_error(&self, k: Option<&super::MapKey>, what: &str) -> Option<String> {
        if !self.namespace {
            return None;
        }
        match k {
            Some(k) if self.is_frozen(k) => Some(format!(
                "cannot assign to constant '{}' (it is a public const of the module)",
                k.display()
            )),
            Some(_) => Some(format!(
                "module namespace is read-only: cannot {} '{}'",
                what,
                k.map(|k| k.display()).unwrap_or_default()
            )),
            None => Some("module namespace is read-only".to_string()),
        }
    }

    #[inline]
    pub fn find(&self, k: &super::MapKey) -> Option<usize> {
        let h = super::fx_hash_map_key(k);
        if self.slots.is_empty() {
            let tag = map_tag(h);
            let n = self.entries.len().min(SMALL_MAP_MAX);
            let tags = &self.small_tags[..n];
            for i in (0..n).rev() {
                if tags[i] == tag && self.entries[i].0 == *k {
                    return Some(i);
                }
            }
            return None;
        }
        let h32 = (h >> 32) as u32;
        let mask = self.slots.len() - 1;
        let mut pos = (h as usize) & mask;
        loop {
            let (sh, idx1) = self.slots[pos];
            if idx1 == 0 {
                return None;
            }
            if sh == h32 && self.entries[(idx1 - 1) as usize].0 == *k {
                return Some((idx1 - 1) as usize);
            }
            pos = (pos + 1) & mask;
        }
    }

    /// Zero-allocation lookup for raw UTF-8 key bytes returning a direct value borrow.
    #[inline(always)]
    pub fn get_str_bytes_with_hash(&self, s: &[u8], h: u64) -> Option<&Value> {
        if self.slots.is_empty() {
            let tag = map_tag(h);
            let n = self.entries.len().min(SMALL_MAP_MAX);
            if n == 0 {
                return None;
            }
            let splat = (tag as u64).wrapping_mul(0x0101_0101_0101_0101);
            let lo = u64::from_le_bytes([
                self.small_tags[0],
                self.small_tags[1],
                self.small_tags[2],
                self.small_tags[3],
                self.small_tags[4],
                self.small_tags[5],
                self.small_tags[6],
                self.small_tags[7],
            ]);
            let x = lo ^ splat;
            let mut mask = x.wrapping_sub(0x0101_0101_0101_0101) & !x & 0x8080_8080_8080_8080;
            if n < 8 {
                mask &= (1u64 << (n * 8)) - 1;
            }
            while mask != 0 {
                let i = (mask.trailing_zeros() >> 3) as usize;
                if let Some((super::MapKey::Str(k), val)) = self.entries.get(i) {
                    if k.eq_bytes(s) {
                        return Some(val);
                    }
                }
                mask &= mask - 1;
            }
            if n > 8 {
                let hi = u64::from_le_bytes([
                    self.small_tags[8],
                    self.small_tags[9],
                    self.small_tags[10],
                    self.small_tags[11],
                    self.small_tags[12],
                    self.small_tags[13],
                    self.small_tags[14],
                    self.small_tags[15],
                ]);
                let x2 = hi ^ splat;
                let mut mask2 =
                    x2.wrapping_sub(0x0101_0101_0101_0101) & !x2 & 0x8080_8080_8080_8080;
                if n < 16 {
                    mask2 &= (1u64 << ((n - 8) * 8)) - 1;
                }
                while mask2 != 0 {
                    let i = 8 + ((mask2.trailing_zeros() >> 3) as usize);
                    if let Some((super::MapKey::Str(k), val)) = self.entries.get(i) {
                        if k.eq_bytes(s) {
                            return Some(val);
                        }
                    }
                    mask2 &= mask2 - 1;
                }
            }
            return None;
        }
        let h32 = (h >> 32) as u32;
        let mask = self.slots.len() - 1;
        let mut pos = (h as usize) & mask;
        loop {
            let (sh, idx1) = self.slots[pos];
            if idx1 == 0 {
                return None;
            }
            if sh == h32 {
                let idx = idx1.wrapping_sub(1) as usize;
                if let Some((super::MapKey::Str(k), val)) = self.entries.get(idx) {
                    if k.eq_bytes(s) {
                        return Some(val);
                    }
                }
            }
            pos = (pos + 1) & mask;
        }
    }

    #[inline]
    pub fn get_str_with_hash(&self, s: &str, h: u64) -> Option<&Value> {
        self.get_str_bytes_with_hash(s.as_bytes(), h)
    }

    #[inline]
    pub fn get_str_bytes(&self, s: &[u8]) -> Option<&Value> {
        self.get_str_bytes_with_hash(s, super::fxhash::fx_hash_bytes(s))
    }

    #[inline]
    pub fn get_str(&self, s: &str) -> Option<&Value> {
        self.get_str_bytes(s.as_bytes())
    }

    /// Zero-allocation lookup for string keys using a caller-provided hash.
    #[inline]
    pub fn find_str_with_hash(&self, s: &str, h: u64) -> Option<usize> {
        self.find_str_bytes_with_hash(s.as_bytes(), h)
    }

    /// Zero-allocation lookup for raw UTF-8 key bytes using a caller-provided hash.
    #[inline]
    pub fn find_str_bytes_with_hash(&self, s: &[u8], h: u64) -> Option<usize> {
        if self.slots.is_empty() {
            let tag = map_tag(h);
            let n = self.entries.len().min(SMALL_MAP_MAX);
            let tags = &self.small_tags[..n];
            for i in (0..n).rev() {
                if tags[i] == tag {
                    if let super::MapKey::Str(k) = &self.entries[i].0 {
                        if k.as_bytes() == s {
                            return Some(i);
                        }
                    }
                }
            }
            return None;
        }
        let h32 = (h >> 32) as u32;
        let mask = self.slots.len() - 1;
        let mut pos = (h as usize) & mask;
        loop {
            let (sh, idx1) = self.slots[pos];
            if idx1 == 0 {
                return None;
            }
            if sh == h32 {
                if let super::MapKey::Str(k) = &self.entries[(idx1 - 1) as usize].0 {
                    if k.as_bytes() == s {
                        return Some((idx1 - 1) as usize);
                    }
                }
            }
            pos = (pos + 1) & mask;
        }
    }

    /// Zero-allocation lookup for string keys (Track 3 RT-3).
    #[inline]
    pub fn find_str(&self, s: &str) -> Option<usize> {
        self.find_str_bytes(s.as_bytes())
    }

    /// Zero-allocation lookup for raw UTF-8 key bytes.
    #[inline]
    pub fn find_str_bytes(&self, s: &[u8]) -> Option<usize> {
        self.find_str_bytes_with_hash(s, super::fxhash::fx_hash_bytes(s))
    }

    pub(crate) fn clear_for_pool(&mut self, out: &mut Vec<Value>) -> bool {
        if self.gc_tracked
            || self.namespace
            || self.frozen.is_some()
            || self.entries.capacity() > 32
        {
            return false;
        }
        if self.entries.iter().all(|(_, v)| is_non_recursive_scalar(v)) {
            self.entries.clear();
        } else {
            for (_, v) in self.entries.drain(..) {
                if !is_non_recursive_scalar(&v) {
                    out.push(v);
                }
            }
        }
        self.slots.clear();
        self.dense_sso_i64 = None;
        self.generation = 0;
        true
    }

    #[inline]
    pub fn set_key(&mut self, k: super::MapKey, val: Value) {
        self.bump_generation();
        let h = super::fx_hash_map_key(&k);
        if self.slots.is_empty() {
            let tag = map_tag(h);
            for i in (0..self.entries.len()).rev() {
                if self.small_tags[i] == tag && self.entries[i].0 == k {
                    self.entries[i].1 = val;
                    return;
                }
            }
            let idx = self.entries.len();
            self.entries.push((k, val));
            if idx < SMALL_MAP_MAX {
                self.small_tags[idx] = tag;
            } else {
                self.rebuild_slots();
            }
            return;
        }
        let h32 = (h >> 32) as u32;
        let mask = self.slots.len() - 1;
        let mut pos = (h as usize) & mask;
        loop {
            let (sh, idx1) = self.slots[pos];
            if idx1 == 0 {
                let idx = self.entries.len();
                self.entries.push((k, val));
                if self.entries.len() * 10 >= self.slots.len() * 7 {
                    self.rebuild_slots();
                } else {
                    self.slots[pos] = (h32, (idx + 1) as u32);
                }
                return;
            }
            if sh == h32 && self.entries[(idx1 - 1) as usize].0 == k {
                self.entries[(idx1 - 1) as usize].1 = val;
                return;
            }
            pos = (pos + 1) & mask;
        }
    }

    /// Remove entry `i` (preserving insertion order of remaining entries) and
    /// update the inline tags or open-addressed index.
    fn remove_at(&mut self, _k: &super::MapKey, i: usize) -> Value {
        let v = self.entries.remove(i).1;
        if self.slots.is_empty() {
            for j in i..self.entries.len() {
                self.small_tags[j] = self.small_tags[j + 1];
            }
        } else if self.entries.len() <= SMALL_MAP_MAX {
            self.slots.clear();
            for (j, (ek, _)) in self.entries.iter().enumerate() {
                self.small_tags[j] = map_tag(super::fx_hash_map_key(ek));
            }
        } else {
            self.rebuild_slots();
        }
        self.bump_generation();
        v
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
                if let Some(e) = self.mutation_error(None, "clear") {
                    return Err(e);
                }
                self.entries.clear();
                self.slots.clear();
                self.bump_generation();
                Ok(Value::Null)
            }
            "set" => {
                want(name, args, 2)?;
                let k = as_key(&args[0])?;
                if let Some(e) = self.mutation_error(Some(&k), "assign") {
                    return Err(e);
                }
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
                if let Some(e) = self.mutation_error(Some(&k), "remove") {
                    return Err(e);
                }
                match self.find(&k) {
                    Some(i) => Ok(self.remove_at(&k, i)),
                    None => Err(format!("missing key {}", k.display())),
                }
            }
            "delete" => {
                want(name, args, 1)?;
                let k = as_key(&args[0])?;
                if let Some(e) = self.mutation_error(Some(&k), "delete") {
                    return Err(e);
                }
                match self.find(&k) {
                    Some(i) => {
                        self.remove_at(&k, i);
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
                Ok(Value::List(new_ref(VecData::from_items(
                    self.entries
                        .iter()
                        .map(|(k, v)| Value::Tuple(new_ref(vec![key_to_value(k), v.clone()])))
                        .collect(),
                ))))
            }
            "contains" => {
                want(name, args, 1)?;
                let k = as_key(&args[0])?;
                Ok(Value::Bool(self.find(&k).is_some()))
            }
            "keys" => {
                want(name, args, 0)?;
                Ok(Value::List(new_ref(VecData::from_items(
                    self.entries.iter().map(|(k, _)| key_to_value(k)).collect(),
                ))))
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

pub fn is_builtin_map_method(name: &str) -> bool {
    matches!(
        name,
        "get"
            | "set"
            | "remove"
            | "delete"
            | "has"
            | "items"
            | "contains"
            | "keys"
            | "values"
            | "clone"
            | "len"
            | "is_empty"
            | "clear"
    )
}

/// `map.values()` list built from an entries snapshot. Method dispatch used to
/// run this `deep_clone` loop while the map's mutex was held; a map containing
/// itself as a value re-locked it and hung (audit F3/p05). Callers snapshot
/// `entries` under the lock and build the result here without it.
pub fn map_values_out(entries: &[(super::MapKey, Value)]) -> Value {
    Value::List(new_ref(VecData::from_items(
        entries.iter().map(|(_, v)| deep_clone(v)).collect(),
    )))
}

pub(crate) const MAP_POOL_CAP: usize = 4096;
pub(crate) const LIST_POOL_CAP: usize = 512;

thread_local! {
    static MAP_POOL: std::cell::RefCell<Vec<crate::value::Ref<MapData>>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static LIST_POOL: std::cell::RefCell<Vec<crate::value::Ref<VecData>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Borrow both thread-local untracked container pools once for a parse call.
#[inline]
pub fn with_container_pools<R>(
    f: impl FnOnce(&mut Vec<crate::value::Ref<MapData>>, &mut Vec<crate::value::Ref<VecData>>) -> R,
) -> R {
    MAP_POOL.with(|mcell| {
        LIST_POOL.with(|lcell| {
            let mut fb_m = Vec::new();
            let mut fb_l = Vec::new();
            let mut mg = mcell.try_borrow_mut();
            let mut lg = lcell.try_borrow_mut();
            let mp = match &mut mg {
                Ok(v) => &mut **v,
                Err(_) => &mut fb_m,
            };
            let lp = match &mut lg {
                Ok(v) => &mut **v,
                Err(_) => &mut fb_l,
            };
            f(mp, lp)
        })
    })
}

/// Pop a uniquely-owned untracked `MapData` `Arc` directly from `pool` (or allocate one).
#[inline]
pub fn pop_pooled_map_arc(
    pool: &mut Vec<crate::value::Ref<MapData>>,
) -> crate::value::Ref<MapData> {
    pool.pop().unwrap_or_else(|| {
        crate::value::new_untracked_ref(MapData::from_entries_untracked(
            Vec::with_capacity(4),
            [0; SMALL_MAP_MAX],
        ))
    })
}

/// Discard partial entries on a failed pooled-map parse and return `arc` to `pool` if eligible.
pub fn recycle_aborted_map_arc(
    pool: &mut Vec<crate::value::Ref<MapData>>,
    mut arc: crate::value::Ref<MapData>,
) {
    let mut can_push = false;
    if let Some(fm) = std::sync::Arc::get_mut(&mut arc) {
        let d = fm.get_mut().unwrap_or_else(|e| e.into_inner());
        let mut work = Vec::new();
        if !d.entries.iter().all(|(_, v)| is_non_recursive_scalar(v)) {
            for (_, v) in d.entries.drain(..) {
                if !is_non_recursive_scalar(&v) {
                    work.push(v);
                }
            }
        } else {
            d.entries.clear();
        }
        if !work.is_empty() {
            super::drop_iteratively(work);
        }
        d.slots.clear();
        d.dense_sso_i64 = None;
        d.namespace = false;
        d.gc_tracked = false;
        d.frozen = None;
        d.generation = 0;
        can_push = d.entries.capacity() <= 32 && pool.len() < effective_map_pool_cap();
        if can_push {
            fm.reset_owner_tid();
        }
    }
    if can_push {
        pool.push(arc);
    }
}

/// Pop a uniquely-owned untracked `VecData` `Arc` directly from `pool` (or allocate one).
#[inline]
pub fn pop_pooled_list_arc(
    pool: &mut Vec<crate::value::Ref<VecData>>,
) -> crate::value::Ref<VecData> {
    pool.pop().unwrap_or_else(|| {
        crate::value::new_untracked_ref(VecData::from_items_untracked(Vec::with_capacity(8)))
    })
}

/// Discard partial items on a failed pooled-list parse and return `arc` to `pool` if eligible.
pub fn recycle_aborted_list_arc(
    pool: &mut Vec<crate::value::Ref<VecData>>,
    mut arc: crate::value::Ref<VecData>,
) {
    let mut can_push = false;
    if let Some(fm) = std::sync::Arc::get_mut(&mut arc) {
        let d = fm.get_mut().unwrap_or_else(|e| e.into_inner());
        let mut work = Vec::new();
        if !d.items.iter().all(is_non_recursive_scalar) {
            for v in d.items.drain(..) {
                if !is_non_recursive_scalar(&v) {
                    work.push(v);
                }
            }
        } else {
            d.items.clear();
        }
        if !work.is_empty() {
            super::drop_iteratively(work);
        }
        d.gen = 0;
        d.dense_i64 = None;
        d.dense_f64 = None;
        d.gc_tracked = false;
        can_push = d.items.capacity() <= 4096 && pool.len() < effective_list_pool_cap();
        if can_push {
            fm.reset_owner_tid();
        }
    }
    if can_push {
        pool.push(arc);
    }
}

#[inline]
fn effective_map_pool_cap() -> usize {
    if crate::value::is_multithreaded() {
        64
    } else {
        MAP_POOL_CAP
    }
}

#[inline]
fn effective_list_pool_cap() -> usize {
    if crate::value::is_multithreaded() {
        32
    } else {
        LIST_POOL_CAP
    }
}

#[inline]
pub fn push_pooled_map(arc: crate::value::Ref<MapData>) {
    let cap = effective_map_pool_cap();
    MAP_POOL.with(|cell| {
        if let Ok(mut pool) = cell.try_borrow_mut() {
            if pool.len() < cap {
                pool.push(arc);
            }
        }
    });
}

#[inline]
pub fn push_pooled_list(arc: crate::value::Ref<VecData>) {
    let cap = effective_list_pool_cap();
    LIST_POOL.with(|cell| {
        if let Ok(mut pool) = cell.try_borrow_mut() {
            if pool.len() < cap {
                pool.push(arc);
            }
        }
    });
}

/// Check out a recycled untracked `MapData` `Arc` and its pre-allocated `entries` buffer.
#[inline]
pub fn checkout_pooled_map() -> (crate::value::Ref<MapData>, Vec<(super::MapKey, Value)>) {
    let pooled = MAP_POOL.with(|cell| cell.try_borrow_mut().ok().and_then(|mut p| p.pop()));
    if let Some(mut arc) = pooled {
        if let Some(fm) = std::sync::Arc::get_mut(&mut arc) {
            fm.reset_owner_tid();
            let d = fm.get_mut().unwrap_or_else(|e| e.into_inner());
            let entries = std::mem::take(&mut d.entries);
            return (arc, entries);
        }
    }
    (
        crate::value::new_untracked_ref(MapData::from_entries_untracked(
            Vec::new(),
            [0; SMALL_MAP_MAX],
        )),
        Vec::with_capacity(4),
    )
}

/// Populate `arc` with parsed entries and return `Value::Map(arc)`.
#[inline]
pub fn commit_pooled_map(
    mut arc: crate::value::Ref<MapData>,
    entries: Vec<(super::MapKey, Value)>,
    small_tags: [u8; SMALL_MAP_MAX],
) -> Value {
    if let Some(fm) = std::sync::Arc::get_mut(&mut arc) {
        let d = fm.get_mut().unwrap_or_else(|e| e.into_inner());
        d.entries = entries;
        d.small_tags = small_tags;
        d.slots.clear();
        d.dense_sso_i64 = None;
        d.namespace = false;
        d.gc_tracked = false;
        d.frozen = None;
        d.generation = 0;
        if d.entries.len() > SMALL_MAP_MAX {
            d.rebuild_slots();
        }
        return Value::Map(arc);
    }
    Value::Map(crate::value::new_untracked_ref(
        MapData::from_entries_untracked(entries, small_tags),
    ))
}

/// Return a checked-out map and discard partial entries on parse failure.
pub fn return_pooled_map(
    mut arc: crate::value::Ref<MapData>,
    mut entries: Vec<(super::MapKey, Value)>,
) {
    if !entries.iter().all(|(_, v)| is_non_recursive_scalar(v)) {
        let mut work = Vec::new();
        for (_, v) in entries.drain(..) {
            if !is_non_recursive_scalar(&v) {
                work.push(v);
            }
        }
        if !work.is_empty() {
            super::drop_iteratively(work);
        }
    } else {
        entries.clear();
    }
    if entries.capacity() <= 32 {
        if let Some(fm) = std::sync::Arc::get_mut(&mut arc) {
            fm.reset_owner_tid();
            let d = fm.get_mut().unwrap_or_else(|e| e.into_inner());
            d.entries = entries;
            d.slots.clear();
            d.dense_sso_i64 = None;
            d.namespace = false;
            d.gc_tracked = false;
            d.frozen = None;
            d.generation = 0;
            push_pooled_map(arc);
        }
    }
}

/// Check out a recycled untracked `VecData` `Arc` and its pre-allocated `items` buffer.
#[inline]
pub fn checkout_pooled_list() -> (crate::value::Ref<VecData>, Vec<Value>) {
    let pooled = LIST_POOL.with(|cell| cell.try_borrow_mut().ok().and_then(|mut p| p.pop()));
    if let Some(mut arc) = pooled {
        if let Some(fm) = std::sync::Arc::get_mut(&mut arc) {
            fm.reset_owner_tid();
            let d = fm.get_mut().unwrap_or_else(|e| e.into_inner());
            let items = std::mem::take(&mut d.items);
            return (arc, items);
        }
    }
    (
        crate::value::new_untracked_ref(VecData::from_items_untracked(Vec::new())),
        Vec::with_capacity(8),
    )
}

/// Populate `arc` with parsed items and return `Value::List(arc)`.
#[inline]
pub fn commit_pooled_list(mut arc: crate::value::Ref<VecData>, items: Vec<Value>) -> Value {
    if let Some(fm) = std::sync::Arc::get_mut(&mut arc) {
        let d = fm.get_mut().unwrap_or_else(|e| e.into_inner());
        d.items = items;
        d.gen = 0;
        d.dense_i64 = None;
        d.dense_f64 = None;
        d.gc_tracked = false;
        return Value::List(arc);
    }
    Value::List(crate::value::new_untracked_ref(
        VecData::from_items_untracked(items),
    ))
}

/// Return a checked-out list and discard partial items on parse failure.
pub fn return_pooled_list(mut arc: crate::value::Ref<VecData>, mut items: Vec<Value>) {
    if !items.iter().all(is_non_recursive_scalar) {
        let mut work = Vec::new();
        for v in items.drain(..) {
            if !is_non_recursive_scalar(&v) {
                work.push(v);
            }
        }
        if !work.is_empty() {
            super::drop_iteratively(work);
        }
    } else {
        items.clear();
    }
    if items.capacity() <= 4096 {
        if let Some(fm) = std::sync::Arc::get_mut(&mut arc) {
            fm.reset_owner_tid();
            let d = fm.get_mut().unwrap_or_else(|e| e.into_inner());
            d.items = items;
            d.gen = 0;
            d.dense_i64 = None;
            d.dense_f64 = None;
            d.gc_tracked = false;
            push_pooled_list(arc);
        }
    }
}

/// Transitively register `root` and any reachable untracked JSON-parsed containers
/// with the cycle GC. Each mutex lock is dropped before visiting child cells.
pub fn track_reachable_untracked(root: &Value) {
    if is_non_recursive_scalar(root) {
        return;
    }
    let mut stack = vec![root.clone()];
    while let Some(v) = stack.pop() {
        match v {
            Value::List(d) | Value::VecDyn(d) => {
                let extra = {
                    let mut g = crate::value::lock_ref(&d);
                    if g.gc_tracked {
                        None
                    } else {
                        g.gc_tracked = true;
                        Some(
                            g.as_slice()
                                .iter()
                                .filter(|x| !is_non_recursive_scalar(x))
                                .cloned()
                                .collect::<Vec<_>>(),
                        )
                    }
                };
                if let Some(children) = extra {
                    crate::value::gc::track(d);
                    stack.extend(children);
                }
            }
            Value::Map(m) => {
                let extra = {
                    let mut g = crate::value::lock_ref(&m);
                    if g.gc_tracked {
                        None
                    } else {
                        g.gc_tracked = true;
                        Some(
                            g.entries
                                .iter()
                                .filter(|(_, x)| !is_non_recursive_scalar(x))
                                .map(|(_, x)| x.clone())
                                .collect::<Vec<_>>(),
                        )
                    }
                };
                if let Some(children) = extra {
                    crate::value::gc::track(m);
                    stack.extend(children);
                }
            }
            Value::Option(Some(inner)) if !is_non_recursive_scalar(&inner) => {
                stack.push(*inner);
            }
            Value::Result(r) => match *r {
                Ok(inner) | Err(inner) if !is_non_recursive_scalar(&inner) => {
                    stack.push(inner);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// Ensure a non-scalar container inserted into another container is GC-tracked.
#[inline]
pub fn track_if_untracked(val: &Value) {
    match val {
        Value::List(d) | Value::VecDyn(d) => {
            if !crate::value::lock_ref(d).gc_tracked {
                track_reachable_untracked(val);
            }
        }
        Value::Map(m) => {
            if !crate::value::lock_ref(m).gc_tracked {
                track_reachable_untracked(val);
            }
        }
        Value::Option(Some(inner)) => track_if_untracked(inner),
        Value::Result(r) => match r.as_ref() {
            Ok(inner) | Err(inner) => track_if_untracked(inner),
        },
        _ => {}
    }
}

// --------------------------------------------------------------------- set

/// Canonical hash for `SetData` scalar side-index (`Int`, `Str`, `Bool` only).
/// `Float` is excluded because ±0.0 and `2.0 == 2` cross-equality would
/// require canonicalizing numeric values across types; `has_float` triggers
/// linear fallback when querying `Int` against a set containing `Float`s.
#[inline]
fn scalar_set_hash(v: &Value) -> Option<u64> {
    use std::hash::Hasher;
    let mut h = super::FxHasher::default();
    match v {
        Value::Int(i) => {
            h.write_u8(1);
            h.write_u64(*i as u64);
            Some(h.finish())
        }
        Value::Str(s) => {
            h.write_u8(2);
            h.write(s.as_bytes());
            Some(h.finish())
        }
        Value::Bool(b) => {
            h.write_u8(3);
            h.write_u8(*b as u8);
            Some(h.finish())
        }
        _ => None,
    }
}

/// Unique membership, insertion order kept for display and iteration.
#[derive(Debug, Default)]
pub struct SetData {
    pub items: Vec<Value>,
    /// Side-index over `Int`/`Str`/`Bool` items -> positions in `items`.
    /// Every hit is confirmed via `values_equal`; misses fall back to linear
    /// scan when `Float` cross-equality may apply (`has_float`).
    index: super::FxHashMap<u64, Vec<usize>>,
    has_float: bool,
}

impl Drop for SetData {
    fn drop(&mut self) {
        // F4/A-17: iterative teardown through the shared helper.
        super::drop_iteratively(std::mem::take(&mut self.items));
    }
}

impl SetData {
    pub fn from_items(items: Vec<Value>) -> SetData {
        let mut s = SetData {
            items: Vec::with_capacity(items.len()),
            index: super::FxHashMap::default(),
            has_float: false,
        };
        for item in items {
            s.push_unchecked(item);
        }
        s
    }

    pub(crate) fn push_unchecked(&mut self, v: Value) {
        let idx = self.items.len();
        if matches!(v, Value::Float(_)) {
            self.has_float = true;
        }
        if let Some(h) = scalar_set_hash(&v) {
            self.index.entry(h).or_default().push(idx);
        }
        self.items.push(v);
    }

    pub(crate) fn remove_at(&mut self, i: usize) {
        let removed = self.items.remove(i);
        if let Some(h) = scalar_set_hash(&removed) {
            if let Some(bucket) = self.index.get_mut(&h) {
                if let Some(pos) = bucket.iter().position(|&idx| idx == i) {
                    bucket.swap_remove(pos);
                }
                if bucket.is_empty() {
                    self.index.remove(&h);
                }
            }
        }
        for bucket in self.index.values_mut() {
            for pos in bucket.iter_mut() {
                if *pos > i {
                    *pos -= 1;
                }
            }
        }
        if matches!(removed, Value::Float(_)) {
            self.has_float = self.items.iter().any(|x| matches!(x, Value::Float(_)));
        }
    }

    pub fn find(&self, v: &Value) -> Option<usize> {
        if let Some(h) = scalar_set_hash(v) {
            if let Some(bucket) = self.index.get(&h) {
                for &idx in bucket {
                    if values_equal(&self.items[idx], v) {
                        return Some(idx);
                    }
                }
            }
            if !(matches!(v, Value::Int(_)) && self.has_float) {
                return None;
            }
        }
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
                self.index.clear();
                self.has_float = false;
                Ok(Value::Null)
            }
            "push" | "add" => {
                want(name, args, 1)?;
                if self.find(&args[0]).is_none() {
                    self.push_unchecked(args[0].clone());
                }
                Ok(Value::Null)
            }
            "remove" => {
                want(name, args, 1)?;
                match self.find(&args[0]) {
                    Some(i) => {
                        self.remove_at(i);
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
            "is_subset" => {
                want(name, args, 1)?;
                let other_items = as_set(&args[0])?.items.clone();
                set_op(name, self.items.clone(), other_items)
            }
            "is_superset" => {
                want(name, args, 1)?;
                let other_items = as_set(&args[0])?.items.clone();
                set_op(name, self.items.clone(), other_items)
            }
            "to_list" => {
                want(name, args, 0)?;
                Ok(Value::List(new_ref(VecData::from_items(
                    self.items.iter().map(deep_clone).collect(),
                ))))
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::Set(new_ref(SetData::from_items(
                    self.items.iter().map(deep_clone).collect(),
                ))))
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
pub fn set_op(op: &str, self_items: Vec<Value>, other_items: Vec<Value>) -> Result<Value, String> {
    match op {
        "union" => {
            let mut s = SetData::from_items(self_items);
            for v in other_items.iter() {
                if s.find(v).is_none() {
                    s.push_unchecked(deep_clone(v));
                }
            }
            Ok(Value::Set(new_ref(s)))
        }
        "intersection" | "intersect" => {
            let items: Vec<Value> = self_items
                .iter()
                .filter(|v| other_items.iter().any(|w| values_equal(v, w)))
                .map(deep_clone)
                .collect();
            Ok(Value::Set(new_ref(SetData::from_items(items))))
        }
        "difference" => {
            let items: Vec<Value> = self_items
                .iter()
                .filter(|v| !other_items.iter().any(|w| values_equal(v, w)))
                .map(deep_clone)
                .collect();
            Ok(Value::Set(new_ref(SetData::from_items(items))))
        }
        "is_subset" => {
            let res = self_items
                .iter()
                .all(|x| other_items.iter().any(|y| values_equal(x, y)));
            Ok(Value::Bool(res))
        }
        "is_superset" => {
            let res = other_items
                .iter()
                .all(|y| self_items.iter().any(|x| values_equal(x, y)));
            Ok(Value::Bool(res))
        }
        _ => Err(unknown(op, "set")),
    }
}

fn as_set(v: &Value) -> Result<crate::value::FastMutexGuard<'_, SetData>, String> {
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
        // Report the VALUE, not its type: "found int" for `-1` told the reader
        // nothing.
        Value::Int(i) => Err(format!("{} must not be negative, found {}", who, i)),
        other => Err(format!(
            "{} expects an integer index, found {}",
            who,
            super::type_name(other)
        )),
    }
}

/// Resolve an index for `obj[i]`, allowing Python-style negative indices:
/// `-1` is the last element, `-len` the first.
///
/// Only the `[]` operator accepts these; method arguments (`remove`, `insert`,
/// `slice`) stay non-negative, because they take an index *and* a length in a
/// different order and the asymmetry would be worse than the inconsistency.
pub fn resolve_index(v: &Value, len: usize, who: &str) -> Result<usize, String> {
    let raw = match v {
        Value::Int(i) => *i,
        other => {
            return Err(format!(
                "{} expects an integer index, found {}",
                who,
                super::type_name(other)
            ))
        }
    };
    let resolved = if raw < 0 { raw + len as i64 } else { raw };
    if resolved < 0 || resolved as usize >= len {
        // Report the index the user wrote, so `xs[-5]` on a 3-element list says
        // -5 rather than -2.
        return Err(format!("index {} out of bounds for length {}", raw, len));
    }
    Ok(resolved as usize)
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

/// A *total* order over the values `sort`/`sorted`/`argsort` accept (all
/// numbers, or all strings). [`value_cmp`] answers `None` for NaN and its
/// callers folded that into `Equal`, which is not a valid sort comparator:
/// NaN equal to everything (and huge ints rounding onto the same f64 as a
/// neighboring float) breaks the transitivity Rust's sort relies on, and it
/// panics the host. Here NaN gets a deterministic slot (after every number),
/// and Int↔Float comparisons are exact in i128.
pub fn natural_cmp(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => match (x.is_nan(), y.is_nan()) {
            (true, true) => Equal,
            (true, false) => Greater,
            (false, true) => Less,
            (false, false) => x.partial_cmp(y).unwrap_or(Equal), // only ±0.0
        },
        (Value::Int(x), Value::Float(y)) => int_vs_float_total(*x, *y),
        (Value::Float(x), Value::Int(y)) => int_vs_float_total(*y, *x).reverse(),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        // Unreachable: every caller pre-checks all-numbers or all-strings.
        _ => Equal,
    }
}

/// Exact total ordering between an integer and a float: NaN sorts last,
/// infinities compare by sign, integral floats compare in i128 (never
/// rounding the integer through f64), and non-integral floats — which always
/// have |y| < 2^52 — compare sign-exactly via `x as f64`.
fn int_vs_float_total(x: i64, y: f64) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    if y.is_nan() {
        return Less; // NaN sorts after every number
    }
    if !y.is_finite() {
        return if y > 0.0 { Less } else { Greater };
    }
    if y >= 9_223_372_036_854_775_808.0 {
        return Less; // y ≥ 2^63 > every i64
    }
    if y < -9_223_372_036_854_775_808.0 {
        return Greater; // y < -2^63 ≤ every i64
    }
    let yi = y.trunc();
    if yi != y {
        // Non-integral y: an integer-valued float never equals a fractional
        // one, so the comparison is Some and sign-exact.
        return (x as f64).partial_cmp(&y).unwrap_or(Equal);
    }
    (x as i128).cmp(&(yi as i128))
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
pub fn comparator_ordering(meth: &str, v: &Value) -> Result<std::cmp::Ordering, String> {
    match v {
        Value::Bool(true) => Ok(std::cmp::Ordering::Less),
        Value::Bool(false) => Ok(std::cmp::Ordering::Greater),
        Value::Int(i) => Ok(i.cmp(&0)),
        Value::Float(f) => f
            .partial_cmp(&0.0)
            .ok_or_else(|| format!("{}: comparator returned NaN, which has no order", meth)),
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
    // The pre-check above guarantees the all-numbers/all-strings domain, where
    // `natural_cmp` is a total order (including NaN and huge int/float mixes).
    merge_sort_indices(&mut order, &mut scratch, &mut |i, j| {
        Ok(natural_cmp(&items[i], &items[j]))
    })?;
    Ok(order)
}

/// Shared: lock a `VecData`-shaped value (list or vec) for read-only use.
pub fn vec_items(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::List(d) | Value::VecDyn(d) => {
            // Snapshot under the lock, deep-clone outside it (audit F3: items
            // can reach the container itself and deep_clone re-locks it).
            let items = d.lock().unwrap().clone_items();
            Ok(items.iter().map(deep_clone).collect())
        }
        other => Err(format!(
            "expected a list, found {}",
            super::type_name(other)
        )),
    }
}

pub fn make_list(items: Vec<Value>) -> Value {
    Value::List(new_ref(VecData::from_items(items)))
}

#[cfg(test)]
mod tests {
    //! Deterministic checks for the snapshot/commit guard (audit A-7): the
    //! concurrent-race versions of these cases live in
    //! `tests/stage2_concurrency.rs`, but those are probabilistic. These drive
    //! the guard directly with a stale snapshot, which is exactly the state a
    //! racing task would produce.

    use super::*;

    fn s(v: &str) -> Value {
        Value::Str(v.into())
    }

    #[test]
    fn set_add_with_a_stale_snapshot_reports_conflict_instead_of_duplicating() {
        let set = new_ref(SetData::default());
        // Snapshot taken by "task A" while the set was empty.
        let stale = set.lock().unwrap().items.clone();
        // "Task B" adds the value first.
        set.lock().unwrap().push_unchecked(s("x"));
        // Task A now tries to commit against its stale snapshot.
        let r = set_detached(&set, stale, "add", &[s("x")]);
        assert!(matches!(r, Err(ref e) if e == CONFLICT), "got {:?}", r);
        assert_eq!(set.lock().unwrap().items.len(), 1, "no duplicate");
    }

    #[test]
    fn set_add_commits_when_the_snapshot_is_still_current() {
        let set = new_ref(SetData::default());
        let snapshot = set.lock().unwrap().items.clone();
        let r = set_detached(&set, snapshot, "add", &[s("x")]);
        assert!(r.is_ok(), "uncontended add must succeed: {:?}", r);
        assert_eq!(set.lock().unwrap().items.len(), 1);
    }

    #[test]
    fn set_remove_with_a_shifted_slot_reports_conflict() {
        let set = new_ref(SetData::from_items(vec![s("a"), s("b")]));
        let stale = set.lock().unwrap().items.clone();
        // A concurrent remove shifts "b" into slot 0.
        set.lock().unwrap().remove_at(0);
        let r = set_detached(&set, stale, "remove", &[s("b")]);
        assert!(
            matches!(r, Err(ref e) if e == CONFLICT),
            "must not remove a different element, got {:?}",
            r
        );
        assert_eq!(set.lock().unwrap().items.len(), 1);
    }

    #[test]
    fn list_remove_with_a_shifted_slot_reports_conflict() {
        let list = new_ref(VecData::from_items(vec![s("a"), s("b")]));
        let stale = list.lock().unwrap().clone_items();
        list.lock().unwrap().insert_item(0, s("z"));
        let r = vec_detached(&list, &stale, "remove", &[s("b")]);
        assert!(matches!(r, Err(ref e) if e == CONFLICT), "got {:?}", r);
        // The original element is still there, untouched.
        assert_eq!(list.lock().unwrap().len(), 3);
    }

    #[test]
    fn vecdata_dense_caches_populate_sync_and_invalidate() {
        let mut v = VecData::from_items(vec![Value::Int(10), Value::Int(20)]);
        let (p0, len0, gen0) = v.ensure_dense_i64().expect("all-int list");
        assert_eq!(len0, 2);
        assert_eq!(gen0, 0);
        // SAFETY: `p0` points to `dense_i64` of length 2 before any mutation.
        assert_eq!(unsafe { std::slice::from_raw_parts(p0, len0) }, &[10, 20]);
        assert!(v.ensure_dense_f64().is_none());

        v.push_item(Value::Int(30));
        let (p1, len1, gen1) = v.ensure_dense_i64().expect("still all-int after push");
        assert_eq!(len1, 3);
        assert!(gen1 > gen0);
        // SAFETY: `p1` points to `dense_i64` of length 3 before any mutation.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(p1, len1) },
            &[10, 20, 30]
        );

        v.set_item(1, Value::Int(99));
        let (p2, len2, gen2) = v.ensure_dense_i64().expect("still all-int after set");
        assert_eq!(len2, 3);
        assert!(gen2 > gen1);
        // SAFETY: `p2` points to `dense_i64` of length 3 before any mutation.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(p2, len2) },
            &[10, 99, 30]
        );

        assert!(matches!(v.pop_item(), Some(Value::Int(30))));
        let (_, len3, gen3) = v.ensure_dense_i64().expect("still all-int after pop");
        assert_eq!(len3, 2);
        assert!(gen3 > gen2);

        // Mixing in a string invalidates dense_i64 and returns None.
        v.push_item(s("mixed"));
        assert!(v.ensure_dense_i64().is_none());

        // Resetting to floats populates dense_f64.
        v.set_items(vec![Value::Float(1.5), Value::Float(2.5)]);
        assert!(v.ensure_dense_i64().is_none());
        let (pf, lf, _) = v.ensure_dense_f64().expect("all-float list");
        assert_eq!(lf, 2);
        // SAFETY: `pf` points to `dense_f64` of length 2.
        assert_eq!(unsafe { std::slice::from_raw_parts(pf, lf) }, &[1.5, 2.5]);

        // Reversal invalidates dense_f64 and recomputes the reversed slice cleanly.
        v.method("reverse", &[]).unwrap();
        let (pf2, lf2, _) = v.ensure_dense_f64().expect("reversed float list");
        // SAFETY: `pf2` points to `dense_f64` of length 2.
        assert_eq!(unsafe { std::slice::from_raw_parts(pf2, lf2) }, &[2.5, 1.5]);

        v.clear_items();
        let (_, lf3, _) = v
            .ensure_dense_f64()
            .expect("empty list has empty dense view");
        assert_eq!(lf3, 0);
    }

    #[test]
    fn mapdata_compact_slots_and_prehashed_find_str() {
        let mut m = MapData::from_entries(Vec::new());
        for i in 0..10 {
            let k = format!("k{i}");
            m.set_key(super::super::MapKey::Str(k.into()), Value::Int(i));
        }
        assert!(m.slots.is_empty());
        assert_eq!(
            m.find_str_with_hash("k7", super::super::fx_hash_str("k7")),
            Some(7)
        );
        assert_eq!(
            m.find_str_with_hash("missing", super::super::fx_hash_str("missing")),
            None
        );

        for i in 10..64 {
            let k = format!("key_long_{i}");
            m.set_key(super::super::MapKey::Str(k.into()), Value::Int(i * 10));
        }
        assert!(!m.slots.is_empty());
        for i in 0..10 {
            let k = format!("k{i}");
            let h = super::super::fx_hash_str(&k);
            let idx = m.find_str_with_hash(&k, h).expect("small key still found");
            assert!(matches!(m.entries[idx].1, Value::Int(v) if v == i));
            assert_eq!(m.find(&super::super::MapKey::Str(k.into())), Some(idx));
        }
        for i in 10..64 {
            let k = format!("key_long_{i}");
            let h = super::super::fx_hash_str(&k);
            let idx = m.find_str_with_hash(&k, h).expect("large key found");
            assert!(matches!(m.entries[idx].1, Value::Int(v) if v == i * 10));
        }
        m.set_key(super::super::MapKey::Str("k3".into()), Value::Int(333));
        assert!(matches!(
            m.entries[m.find_str("k3").unwrap()].1,
            Value::Int(333)
        ));
        m.remove_at(&super::super::MapKey::Str("key_long_20".into()), 20);
        assert!(m.find_str("key_long_20").is_none());
        assert_eq!(m.find_str_with_hash("k999", 0), None);
    }

    #[test]
    fn mapdata_swar_small_lookup_and_dense_sso_i64_view() {
        let mut m = MapData::from_entries(Vec::new());
        let mut expected_sum = 0i64;
        for i in 0..16i64 {
            let k = format!("k{i}");
            let v = i * 10;
            expected_sum += v;
            m.set_key(super::super::MapKey::Str(k.into()), Value::Int(v));
        }
        for i in 0..16i64 {
            let k = format!("k{i}");
            assert_eq!(m.find_str(&k), Some(i as usize));
        }
        assert_eq!(m.find_str("k16"), None);
        assert_eq!(m.find_str("missing"), None);

        let (ptr, mask) = m.ensure_dense_sso_i64().expect("dense sso i64 view");
        assert!((mask + 1).is_power_of_two());
        // SAFETY: `ensure_dense_sso_i64` returns a slice of `mask + 1` initialized slots valid until `m` mutates.
        let slots = unsafe { std::slice::from_raw_parts(ptr, mask + 1) };
        let non_empty: Vec<&DenseSsoI64Slot> = slots.iter().filter(|s| s.tag != 0).collect();
        assert_eq!(non_empty.len(), 16);
        assert_eq!(non_empty.iter().map(|s| s.val).sum::<i64>(), expected_sum);

        // Mutating an existing key invalidates and rebuilds the dense mirror.
        m.set_key(super::super::MapKey::Str("k0".into()), Value::Int(999));
        let (ptr2, mask2) = m.ensure_dense_sso_i64().expect("rebuilt dense view");
        // SAFETY: `ensure_dense_sso_i64` returns a slice of `mask2 + 1` initialized slots valid until `m` mutates.
        let slots2 = unsafe { std::slice::from_raw_parts(ptr2, mask2 + 1) };
        let non_empty2: Vec<&DenseSsoI64Slot> = slots2.iter().filter(|s| s.tag != 0).collect();
        assert_eq!(non_empty2.len(), 16);
        assert_eq!(
            non_empty2.iter().map(|s| s.val).sum::<i64>(),
            expected_sum + 999
        );

        // Growing above SMALL_MAP_MAX (16) keeps both `find_str` and dense SSO+i64 valid.
        for i in 16..32i64 {
            let k = format!("k{i}");
            m.set_key(super::super::MapKey::Str(k.into()), Value::Int(i));
        }
        for i in 0..32i64 {
            assert!(m.find_str(&format!("k{i}")).is_some());
        }
        let (ptr3, mask3) = m.ensure_dense_sso_i64().expect("dense view at 32 entries");
        // SAFETY: `ensure_dense_sso_i64` returns a slice of `mask3 + 1` initialized slots valid until `m` mutates.
        let slots3 = unsafe { std::slice::from_raw_parts(ptr3, mask3 + 1) };
        assert_eq!(slots3.iter().filter(|s| s.tag != 0).count(), 32);

        // Non-integer values or >15-byte keys disqualify the dense SSO+i64 mirror.
        m.set_key(
            super::super::MapKey::Str("bad".into()),
            Value::Str("nope".into()),
        );
        assert!(m.ensure_dense_sso_i64().is_none());

        let mut long_key_map = MapData::from_entries(Vec::new());
        long_key_map.set_key(
            super::super::MapKey::Str("0123456789abcdef".into()),
            Value::Int(1),
        );
        assert!(long_key_map.ensure_dense_sso_i64().is_none());

        // Pooled map helpers must clear any cached `dense_sso_i64` mirror.
        let (arc, mut entries) = checkout_pooled_map();
        entries.push((super::super::MapKey::Str("a".into()), Value::Int(42)));
        let Value::Map(committed) = commit_pooled_map(arc, entries, [0; SMALL_MAP_MAX]) else {
            panic!("expected Value::Map");
        };
        assert!(crate::value::lock_ref(&committed)
            .ensure_dense_sso_i64()
            .is_some());
        return_pooled_map(committed, Vec::new());
        let (rechecked, _) = checkout_pooled_map();
        assert!(crate::value::lock_ref(&rechecked).dense_sso_i64.is_none());

        let arc2 = crate::value::new_untracked_ref(MapData::from_entries_untracked(
            vec![(super::super::MapKey::Str("x".into()), Value::Int(7))],
            [0; SMALL_MAP_MAX],
        ));
        assert!(crate::value::lock_ref(&arc2)
            .ensure_dense_sso_i64()
            .is_some());
        let mut pool = Vec::new();
        recycle_aborted_map_arc(&mut pool, arc2);
        let popped = pool.pop().expect("pooled map");
        assert!(crate::value::lock_ref(&popped).dense_sso_i64.is_none());
    }
}
