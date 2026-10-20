//! Collection dispatch: builtin constructors, method calls, string methods,
//! iteration, and length. The interpreter routes every `obj.name(args)` here
//! (except instances, which it dispatches itself).

pub mod misc;
pub mod nodes;
pub mod seq;

use std::collections::VecDeque;
use std::sync::Arc;

use crate::sched::{ChannelData, MutexData};
use crate::value::{deep_clone, new_ref, values_equal, Value};

pub use seq::{make_list, unknown, value_cmp, want, want_index, want_int, want_num, want_str};

// Re-exported so the structure modules can say `super::x`.
pub use crate::value::{as_map_key, display_value, key_to_value, type_name, MapKey};

// Re-exported so `value` can name the structs directly.
pub use misc::{
    BagData, GraphData, HashTableData, HeapData, MatrixData, PrioQueueData, RefBoxData, RingData,
    TrieData, VectorData,
};
pub use nodes::{BinTreeData, BstData, ListData};
pub use seq::{DequeData, MapData, QueueData, SetData, StackData, VecData};

// ------------------------------------------------------------- constructors

pub fn is_constructor(name: &str) -> bool {
    matches!(
        name,
        "list"
            | "vec"
            | "stack"
            | "queue"
            | "deque"
            | "linked_list"
            | "singly_linked_list"
            | "doubly_linked_list"
            | "map"
            | "set"
            | "tuple"
            | "binary_tree"
            | "bst"
            | "heap"
            | "priority_queue"
            | "graph"
            | "hash_table"
            | "trie"
            | "matrix"
            | "matrix_from_list"
            | "vector"
            | "bytes"
            | "ring_buffer"
            | "bag"
            | "ref"
            | "channel"
            | "mutex"
            | "some"
            | "none"
            | "ok"
            | "err"
    )
}

pub fn construct(name: &str, args: Vec<Value>) -> Result<Value, String> {
    match name {
        "list" | "vec" => {
            let d = new_ref(seq::VecData { items: args });
            Ok(if name == "list" {
                Value::List(d)
            } else {
                Value::VecDyn(d)
            })
        }
        "stack" => Ok(Value::Stack(new_ref(seq::StackData { items: args }))),
        "queue" => Ok(Value::Queue(new_ref(seq::QueueData {
            items: args.into_iter().collect(),
        }))),
        "deque" => Ok(Value::Deque(new_ref(seq::DequeData {
            items: args.into_iter().collect(),
        }))),
        "linked_list" => {
            let mut d = nodes::ListData::linked();
            for a in args {
                d.push_back(a);
            }
            Ok(nodes::new_list_value(d, false, true))
        }
        "singly_linked_list" => {
            let mut d = nodes::ListData::singly();
            for a in args {
                d.push_back(a);
            }
            Ok(nodes::new_list_value(d, false, false))
        }
        "doubly_linked_list" => {
            let mut d = nodes::ListData::doubly();
            for a in args {
                d.push_back(a);
            }
            Ok(nodes::new_list_value(d, true, true))
        }
        "map" => {
            if !args.len().is_multiple_of(2) {
                return Err(format!(
                    "map expects key, value pairs; got {} arguments",
                    args.len()
                ));
            }
            let mut entries = Vec::new();
            let mut it = args.into_iter();
            while let (Some(k), Some(v)) = (it.next(), it.next()) {
                let key = crate::value::as_map_key(&k)?;
                if let Some(slot) = entries
                    .iter_mut()
                    .find(|(ek, _): &&mut (crate::value::MapKey, Value)| *ek == key)
                {
                    slot.1 = v;
                } else {
                    entries.push((key, v));
                }
            }
            Ok(Value::Map(new_ref(seq::MapData::from_entries(entries))))
        }
        "set" => {
            let mut items: Vec<Value> = Vec::new();
            for a in args {
                if !items.iter().any(|v| values_equal(v, &a)) {
                    items.push(a);
                }
            }
            Ok(Value::Set(new_ref(seq::SetData { items })))
        }
        "tuple" => Ok(Value::Tuple(new_ref(args))),
        "binary_tree" => Ok(Value::BinTree(new_ref(nodes::BinTreeData::new()))),
        "bst" => Ok(Value::Bst(new_ref(nodes::BstData::new()))),
        "heap" => {
            let s = want_str(&one(name, &args)?, "heap")?;
            let min = misc::parse_direction(&s)?;
            Ok(Value::Heap(new_ref(misc::HeapData::new(min))))
        }
        "priority_queue" => {
            let min = if args.is_empty() {
                true
            } else {
                misc::parse_direction(&want_str(&args[0], "priority_queue")?)?
            };
            Ok(Value::PrioQueue(new_ref(misc::PrioQueueData::new(min))))
        }
        "graph" => {
            let s = want_str(&one(name, &args)?, "graph")?;
            let directed = match s.as_str() {
                "directed" => true,
                "undirected" => false,
                other => {
                    return Err(format!(
                        "graph expects \"directed\" or \"undirected\", found \"{}\"",
                        other
                    ))
                }
            };
            Ok(Value::Graph(new_ref(misc::GraphData::new(directed))))
        }
        "hash_table" => Ok(Value::HashTable(new_ref(misc::HashTableData::new()))),
        "trie" => Ok(Value::Trie(new_ref(misc::TrieData::new()))),
        "vector" => {
            if args.is_empty() {
                Ok(Value::Vector(new_ref(misc::VectorData { data: Vec::new() })))
            } else if args.len() == 1 {
                match &args[0] {
                    Value::List(l) | Value::VecDyn(l) => {
                        let items = l.lock().unwrap().items.clone();
                        if items.len() > misc::MAX_VECTOR_ELEMENTS {
                            return Err(format!(
                                "vector of {} elements is too large (max {})",
                                items.len(),
                                misc::MAX_VECTOR_ELEMENTS
                            ));
                        }
                        let mut data = Vec::with_capacity(items.len());
                        for it in &items {
                            data.push(want_num(it, "vector")?);
                        }
                        Ok(Value::Vector(new_ref(misc::VectorData { data })))
                    }
                    Value::Tuple(t) => {
                        let items = t.lock().unwrap().clone();
                        if items.len() > misc::MAX_VECTOR_ELEMENTS {
                            return Err(format!(
                                "vector of {} elements is too large (max {})",
                                items.len(),
                                misc::MAX_VECTOR_ELEMENTS
                            ));
                        }
                        let mut data = Vec::with_capacity(items.len());
                        for it in &items {
                            data.push(want_num(it, "vector")?);
                        }
                        Ok(Value::Vector(new_ref(misc::VectorData { data })))
                    }
                    Value::Vector(v) => {
                        let data = v.lock().unwrap().data.clone();
                        Ok(Value::Vector(new_ref(misc::VectorData { data })))
                    }
                    Value::Int(n) => {
                        if *n < 0 {
                            return Err("vector length must be non-negative".to_string());
                        }
                        let nu = *n as usize;
                        if nu > misc::MAX_VECTOR_ELEMENTS {
                            return Err(format!(
                                "vector of {} elements is too large (max {})",
                                nu,
                                misc::MAX_VECTOR_ELEMENTS
                            ));
                        }
                        Ok(Value::Vector(new_ref(misc::VectorData {
                            data: vec![0.0; nu],
                        })))
                    }
                    other => Err(format!(
                        "vector expects an int length or list of numbers, found {}",
                        super::type_name(other)
                    )),
                }
            } else if args.len() == 2 {
                let n = want_int(&args[0], "vector")?;
                if n < 0 {
                    return Err("vector length must be non-negative".to_string());
                }
                let nu = n as usize;
                if nu > misc::MAX_VECTOR_ELEMENTS {
                    return Err(format!(
                        "vector of {} elements is too large (max {})",
                        nu,
                        misc::MAX_VECTOR_ELEMENTS
                    ));
                }
                let fill = want_num(&args[1], "vector")?;
                Ok(Value::Vector(new_ref(misc::VectorData {
                    data: vec![fill; nu],
                })))
            } else {
                Err(format!("vector expects at most 2 arguments, got {}", args.len()))
            }
        }
        "bytes" => {
            if args.is_empty() {
                Ok(Value::Bytes(new_ref(Vec::new())))
            } else if args.len() == 1 {
                match &args[0] {
                    Value::Int(n) => {
                        if *n < 0 {
                            return Err("bytes length must be non-negative".to_string());
                        }
                        let nu = *n as usize;
                        if nu > 16_777_216 {
                            return Err(format!("bytes size {} is too large (max 16777216)", nu));
                        }
                        Ok(Value::Bytes(new_ref(vec![0u8; nu])))
                    }
                    Value::List(l) | Value::VecDyn(l) => {
                        let items = l.lock().unwrap().items.clone();
                        let mut data = Vec::with_capacity(items.len());
                        for it in items {
                            let b = want_int(&it, "bytes")?;
                            if !(0..=255).contains(&b) {
                                return Err(format!("byte value must be between 0 and 255, got {}", b));
                            }
                            data.push(b as u8);
                        }
                        Ok(Value::Bytes(new_ref(data)))
                    }
                    Value::Tuple(t) => {
                        let items = t.lock().unwrap().clone();
                        let mut data = Vec::with_capacity(items.len());
                        for it in items {
                            let b = want_int(&it, "bytes")?;
                            if !(0..=255).contains(&b) {
                                return Err(format!("byte value must be between 0 and 255, got {}", b));
                            }
                            data.push(b as u8);
                        }
                        Ok(Value::Bytes(new_ref(data)))
                    }
                    Value::Str(s) => Ok(Value::Bytes(new_ref(s.as_bytes().to_vec()))),
                    Value::Bytes(b) => {
                        let data = b.lock().unwrap().clone();
                        Ok(Value::Bytes(new_ref(data)))
                    }
                    other => Err(format!(
                        "bytes expects an integer size, list of byte ints, string, or bytes; found {}",
                        super::type_name(other)
                    )),
                }
            } else if args.len() == 2 {
                let n = want_int(&args[0], "bytes")?;
                if n < 0 {
                    return Err("bytes length must be non-negative".to_string());
                }
                let nu = n as usize;
                if nu > 16_777_216 {
                    return Err(format!("bytes size {} is too large (max 16777216)", nu));
                }
                let fill = want_int(&args[1], "bytes")?;
                if !(0..=255).contains(&fill) {
                    return Err(format!("byte value must be between 0 and 255, got {}", fill));
                }
                Ok(Value::Bytes(new_ref(vec![fill as u8; nu])))
            } else {
                Err(format!("bytes expects at most 2 arguments, got {}", args.len()))
            }
        }
        "matrix_from_list" => {
            want("matrix_from_list", &args, 1)?;
            misc::matrix_from_list_val(&args[0])
        }
        "matrix" => {
            if args.len() == 1 && matches!(&args[0], Value::List(_) | Value::VecDyn(_)) {
                return misc::matrix_from_list_val(&args[0]);
            }
            if args.len() < 2 || args.len() > 3 {
                return Err(format!(
                    "matrix expects rows, cols, [fill]; got {} args",
                    args.len()
                ));
            }
            let rows = want_int(&args[0], "matrix")?;
            let cols = want_int(&args[1], "matrix")?;
            let fill = if args.len() == 3 {
                want_num(&args[2], "matrix")?
            } else {
                0.0
            };
            if rows < 0 || cols < 0 {
                return Err("matrix dimensions must be non-negative".to_string());
            }
            let (ru, cu) = (rows as usize, cols as usize);
            misc::check_matrix_dims(ru, cu)?;
            Ok(Value::Matrix(new_ref(misc::MatrixData::new(ru, cu, fill))))
        }
        "ring_buffer" => {
            let cap = want_int(&one(name, &args)?, "ring_buffer")?;
            if cap < 1 {
                return Err("ring_buffer capacity must be at least 1".to_string());
            }
            Ok(Value::Ring(new_ref(misc::RingData {
                cap: cap as usize,
                items: VecDeque::new(),
            })))
        }
        "bag" => Ok(Value::Bag(new_ref(misc::BagData::default()))),
        "ref" => Ok(Value::RefBox(new_ref(misc::RefBoxData {
            value: one(name, &args)?,
        }))),
        "channel" => Ok(Value::Channel(Arc::new(ChannelData::new()))),
        "mutex" => Ok(Value::MutexVal(Arc::new(MutexData::new(
            one(name, &args)?,
        )))),
        "some" => Ok(Value::Option(Some(Box::new(one(name, &args)?)))),
        "none" => {
            if !args.is_empty() {
                return Err("none takes no arguments".to_string());
            }
            Ok(Value::Option(None))
        }
        "ok" => Ok(Value::Result(Ok(Box::new(one(name, &args)?)))),
        "err" => Ok(Value::Result(Err(Box::new(one(name, &args)?)))),
        _ => Err(format!("unknown constructor '{}'", name)),
    }
}

fn one(name: &str, args: &[Value]) -> Result<Value, String> {
    if args.len() == 1 {
        Ok(args[0].clone())
    } else {
        Err(format!("{} expects 1 argument, got {}", name, args.len()))
    }
}

// -------------------------------------------------------------- method call

/// Who can call a Spid3r function back (for higher-order methods such as
/// `list.map(f)`). Each engine implements this: the tree interpreter calls
/// `Function` values directly, the bytecode VM pushes a frame and runs to its
/// own depth. The native AOT backend and other contexts without a caller use
/// `NoCallbacks`.
pub trait CallbackHost {
    fn call_callback(&mut self, f: &Value, args: Vec<Value>) -> Result<Value, String>;
}

/// Fallback host: rejects callbacks with a clear message instead of silently
/// misbehaving.
pub struct NoCallbacks;

impl CallbackHost for NoCallbacks {
    fn call_callback(&mut self, _f: &Value, _args: Vec<Value>) -> Result<Value, String> {
        Err(
            "higher-order methods with callbacks are not supported in this context \
             (they require the tree interpreter or the bytecode VM)"
                .to_string(),
        )
    }
}

/// The list methods dispatched through [`list_callback_method`]: the ones that
/// take a Spid3r function as an argument (`map`, `filter`, ...), the sort
/// variants that need the same snapshot-before-callback discipline, and
/// `argsort`, which shares the index machinery. Every one of them operates on
/// the snapshot the caller takes — never on the live list.
pub fn is_callback_method(name: &str) -> bool {
    matches!(
        name,
        "map"
            | "filter"
            | "reduce"
            | "for_each"
            | "all"
            | "any"
            | "sort_by"
            | "sorted_by"
            | "argsort"
    )
}

pub fn call_method(obj: &Value, name: &str, args: Vec<Value>) -> Result<Value, String> {
    call_method_host(&mut NoCallbacks, obj, name, args)
}

pub fn call_method_host(
    host: &mut dyn CallbackHost,
    obj: &Value,
    name: &str,
    args: Vec<Value>,
) -> Result<Value, String> {
    // clone() is universal: deep copy for collections, identity for functions.
    if name == "clone" {
        return Ok(deep_clone(obj));
    }
    match obj {
        Value::List(d) | Value::VecDyn(d) => {
            if is_callback_method(name) {
                // Snapshot the items so the callback cannot mutate the list
                // while it is being iterated (both engines share this rule).
                let items = d.lock().unwrap().items.clone();
                // `obj` is passed through for the in-place variants; it is
                // never locked until every callback has returned.
                return list_callback_method(host, Some(obj), &items, name, &args);
            }
            if name == "append" && args.len() == 1 {
                if let Value::List(o) | Value::VecDyn(o) = &args[0] {
                    if std::sync::Arc::ptr_eq(d, o) {
                        let extra = d.lock().unwrap().items.clone();
                        d.lock().unwrap().items.extend(extra);
                        return Ok(Value::Null);
                    }
                }
            }
            d.lock().unwrap().method(name, &args)
        }
        Value::Stack(d) => d.lock().unwrap().method(name, &args),
        Value::Queue(d) => d.lock().unwrap().method(name, &args),
        Value::Deque(d) => d.lock().unwrap().method(name, &args),
        Value::LinkedList(d) | Value::SinglyList(d) | Value::DoublyList(d) => {
            if name == "to_list" && args.is_empty() {
                // Snapshot the node values under the list's lock, then
                // deep-clone outside it: a node value can be the list itself,
                // and deep_clone would re-lock the mutex dispatch already
                // holds (audit F3/p06).
                let vals = d.lock().unwrap().to_vec();
                return Ok(make_list(vals.iter().map(deep_clone).collect()));
            }
            d.lock().unwrap().method(name, &args)
        }
        Value::Map(d) => {
            if name == "values" && args.is_empty() {
                // Snapshot the entries under the map's lock, then deep-clone
                // the values outside it: a value can be the map itself, and
                // deep_clone would re-lock the mutex dispatch already holds
                // (audit F3/p05).
                let entries = d.lock().unwrap().entries.clone();
                return Ok(seq::map_values_out(&entries));
            }
            d.lock().unwrap().method(name, &args)
        }
        Value::Set(d) => {
            if matches!(name, "union" | "intersection" | "intersect" | "difference")
                && args.len() == 1
            {
                if let Value::Set(o) = &args[0] {
                    // Snapshot both sides under their own locks and release
                    // before any values_equal/deep_clone recursion: items can
                    // be (or reach) either set, and the comparison re-locks
                    // both containers — the old ptr_eq self-guard plus the
                    // as_set guard deadlocked on mutually self-referencing
                    // sets (audit F3/p03).
                    let self_items = d.lock().unwrap().items.clone();
                    let other_items = o.lock().unwrap().items.clone();
                    return seq::set_op(name, self_items, other_items);
                }
            }
            d.lock().unwrap().method(name, &args)
        }
        Value::BinTree(d) => d.lock().unwrap().method(name, &args),
        Value::Bst(d) => d.lock().unwrap().method(name, &args),
        Value::Heap(d) => d.lock().unwrap().method(name, &args),
        Value::PrioQueue(d) => d.lock().unwrap().method(name, &args),
        Value::Graph(d) => d.lock().unwrap().method(name, &args),
        Value::HashTable(d) => d.lock().unwrap().method(name, &args),
        Value::Trie(d) => d.lock().unwrap().method(name, &args),
        Value::Matrix(d) => {
            if matches!(name, "add" | "mul" | "sub" | "hadamard" | "solve") && args.len() == 1 {
                if let Value::Matrix(o) = &args[0] {
                    if std::sync::Arc::ptr_eq(d, o) {
                        let snap = {
                            let m = d.lock().unwrap();
                            MatrixData {
                                rows: m.rows,
                                cols: m.cols,
                                data: m.data.clone(),
                            }
                        };
                        return d
                            .lock()
                            .unwrap()
                            .method(name, &[Value::Matrix(new_ref(snap))]);
                    }
                }
            }
            d.lock().unwrap().method(name, &args)
        }
        Value::Vector(d) => {
            if matches!(name, "add" | "sub" | "mul" | "div" | "dot") && args.len() == 1 {
                if let Value::Vector(o) = &args[0] {
                    if std::sync::Arc::ptr_eq(d, o) {
                        let snap = {
                            let v = d.lock().unwrap();
                            misc::VectorData {
                                data: v.data.clone(),
                            }
                        };
                        return d
                            .lock()
                            .unwrap()
                            .method(name, &[Value::Vector(new_ref(snap))]);
                    }
                }
            }
            d.lock().unwrap().method(name, &args)
        }
        Value::Bytes(d) => misc::bytes_method(d, name, &args),
        Value::Ring(d) => d.lock().unwrap().method(name, &args),
        Value::Bag(d) => d.lock().unwrap().method(name, &args),
        Value::RefBox(d) => d.lock().unwrap().method(name, &args),
        Value::Str(s) => string_method(s, name, &args),
        Value::Tuple(t) => misc::tuple_method(&t.lock().unwrap(), name, &args),
        Value::Option(o) => option_method(o.as_deref(), name, &args),
        Value::Result(r) => {
            let rr: Result<&Value, &Value> = match r {
                Ok(v) => Ok(v.as_ref()),
                Err(e) => Err(e.as_ref()),
            };
            result_method(rr, name, &args)
        }
        Value::Task(t) => task_method(t, name, &args),
        Value::Channel(c) => channel_method(c, name, args),
        Value::MutexVal(m) => mutex_method(m, name, &args),
        other => Err(format!(
            "{} values have no methods",
            crate::value::type_name(other)
        )),
    }
}

/// Higher-order list methods. `items` is a snapshot taken by the caller.
/// The callback is invoked through the engine-specific `CallbackHost`.
/// `receiver` is the list the method was called on, when there is one — the
/// in-place variants (`sort_by`) write their result back into it. It is only
/// used AFTER the last callback returns, so no lock is ever held across a
/// call back into Spid3r.
pub fn list_callback_method(
    host: &mut dyn CallbackHost,
    receiver: Option<&Value>,
    items: &[Value],
    name: &str,
    args: &[Value],
) -> Result<Value, String> {
    // Validates that the last of `n` arguments is a callable value.
    fn want_fn<'a>(
        meth: &str,
        args: &'a [Value],
        n: usize,
    ) -> Result<&'a Value, String> {
        if args.len() != n {
            return Err(format!(
                "{} expects {} argument(s), got {}",
                meth,
                n,
                args.len()
            ));
        }
        let f = &args[n - 1];
        match f {
            Value::Function(_) | Value::Native(_) | Value::NativeClosure(_) | Value::VFunc(_) | Value::Cfn(_) => Ok(f),
            other => Err(format!(
                "{} expects a function, found {}",
                meth,
                crate::value::type_name(other)
            )),
        }
    }
    match name {
        "map" => {
            let f = want_fn(name, args, 1)?.clone();
            let out = items
                .iter()
                .map(|it| host.call_callback(&f, vec![it.clone()]))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(make_list(out))
        }
        "filter" => {
            let f = want_fn(name, args, 1)?.clone();
            let mut out = Vec::new();
            for it in items {
                let keep = host.call_callback(&f, vec![it.clone()])?;
                if crate::value::truthy(&keep) {
                    out.push(it.clone());
                }
            }
            Ok(make_list(out))
        }
        "reduce" => {
            let f = want_fn(name, args, 2)?.clone();
            let mut acc = args[0].clone();
            for it in items {
                acc = host.call_callback(&f, vec![acc, it.clone()])?;
            }
            Ok(acc)
        }
        "for_each" => {
            let f = want_fn(name, args, 1)?.clone();
            for it in items {
                host.call_callback(&f, vec![it.clone()])?;
            }
            Ok(Value::Null)
        }
        "all" => {
            let f = want_fn(name, args, 1)?.clone();
            for it in items {
                let ok = host.call_callback(&f, vec![it.clone()])?;
                if !crate::value::truthy(&ok) {
                    return Ok(Value::Bool(false));
                }
            }
            Ok(Value::Bool(true))
        }
        "any" => {
            let f = want_fn(name, args, 1)?.clone();
            for it in items {
                let ok = host.call_callback(&f, vec![it.clone()])?;
                if crate::value::truthy(&ok) {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }
        "sort_by" | "sorted_by" => {
            let f = want_fn(name, args, 1)?.clone();
            // Sort a permutation of the snapshot, not the snapshot's own
            // order: the comparator is arbitrary Spid3r code (it can call
            // back into this same list), so it must run with no lock held.
            let mut order: Vec<usize> = (0..items.len()).collect();
            let mut scratch = vec![0usize; items.len()];
            // A boolean comparator answers only "is `a` less than `b`"; it has
            // no way to say "equal". So when the first answer is `false` the
            // mirror question is asked too, which separates `a > b` from
            // `a == b`. Without that, `false` would have to be read as
            // "greater" and equal elements would come out reversed (and an
            // always-false comparator would reverse the whole list).
            let mut cmp = |ia: usize, ib: usize| -> Result<_, String> {
                let first =
                    host.call_callback(&f, vec![items[ia].clone(), items[ib].clone()])?;
                if !matches!(first, Value::Bool(false)) {
                    return seq::comparator_ordering(name, &first);
                }
                let mirror =
                    host.call_callback(&f, vec![items[ib].clone(), items[ia].clone()])?;
                match &mirror {
                    Value::Bool(false) => Ok(std::cmp::Ordering::Equal),
                    Value::Bool(true) => Ok(std::cmp::Ordering::Greater),
                    other => seq::comparator_ordering(name, other),
                }
            };
            seq::merge_sort_indices(&mut order, &mut scratch, &mut cmp)?;
            // `cmp`'s borrow of `host` ends here; nothing below touches it.
            let sorted = seq::permuted(items, &order);
            if name == "sorted_by" {
                return Ok(make_list(sorted));
            }
            // In-place: the write-back happens after the last comparator call,
            // so it cannot deadlock against the list the comparator mutated.
            match receiver {
                Some(Value::List(d)) | Some(Value::VecDyn(d)) => {
                    d.lock().unwrap().items = sorted;
                    Ok(Value::Null)
                }
                _ => Err("sort_by needs a list receiver".to_string()),
            }
        }
        "argsort" => {
            if !args.is_empty() {
                return Err(format!(
                    "argsort expects 0 arguments, got {}",
                    args.len()
                ));
            }
            let order = seq::argsort_indices(items)?;
            Ok(make_list(
                order
                    .into_iter()
                    .map(|i| Value::Int(i as i64))
                    .collect::<Vec<_>>(),
            ))
        }
        _ => Err(unknown(name, "list")),
    }
}

// ------------------------------------------------------------ string method

pub fn string_method(s: &str, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "len" => {
            want(name, args, 0)?;
            Ok(Value::Int(s.chars().count() as i64))
        }
        "is_empty" => {
            want(name, args, 0)?;
            Ok(Value::Bool(s.is_empty()))
        }
        "slice" => {
            if args.is_empty() || args.len() > 2 {
                return Err(format!(
                    "slice expects 1 or 2 arguments, got {}",
                    args.len()
                ));
            }
            let chars: Vec<char> = s.chars().collect();
            let a = want_index(&args[0], "string.slice")?;
            let b = if args.len() == 2 {
                want_index(&args[1], "string.slice")?
            } else {
                chars.len()
            };
            if a > b || b > chars.len() {
                return Err(format!(
                    "slice range {}..{} out of bounds for length {}",
                    a,
                    b,
                    chars.len()
                ));
            }
            Ok(Value::Str(chars[a..b].iter().collect::<String>().into()))
        }
        "upper" | "lower" => {
            want(name, args, 0)?;
            Ok(Value::Str(if name == "upper" {
                s.to_uppercase()
            } else {
                s.to_lowercase()
            }
            .into()))
        }
        "trim" => {
            want(name, args, 0)?;
            Ok(Value::Str(s.trim().into()))
        }
        "split" => {
            want(name, args, 1)?;
            let sep = want_str(&args[0], "string.split")?;
            let items: Vec<Value> = if sep.is_empty() {
                s.chars().map(|c| Value::Str(c.to_string().into())).collect()
            } else {
                s.split(&sep).map(|p| Value::Str(p.into())).collect()
            };
            Ok(make_list(items))
        }
        "contains" => {
            want(name, args, 1)?;
            let sub = want_str(&args[0], "string.contains")?;
            Ok(Value::Bool(s.contains(&sub)))
        }
        "starts_with" => {
            want(name, args, 1)?;
            let p = want_str(&args[0], "string.starts_with")?;
            Ok(Value::Bool(s.starts_with(&p)))
        }
        "ends_with" => {
            want(name, args, 1)?;
            let p = want_str(&args[0], "string.ends_with")?;
            Ok(Value::Bool(s.ends_with(&p)))
        }
        "index_of" => {
            want(name, args, 1)?;
            let sub = want_str(&args[0], "string.index_of")?;
            let pos = s
                .find(&sub)
                .map(|b| s[..b].chars().count() as i64)
                .unwrap_or(-1);
            Ok(Value::Int(pos))
        }
        "replace" => {
            want(name, args, 2)?;
            let from = want_str(&args[0], "string.replace")?;
            let to = want_str(&args[1], "string.replace")?;
            Ok(Value::Str(s.replace(&from, &to).into()))
        }
        "repeat" => {
            want(name, args, 1)?;
            let n = want_int(&args[0], "string.repeat")?;
            if n < 0 {
                return Err("repeat count must be non-negative".to_string());
            }
            const MAX_STR_BYTES: usize = 64 * 1024 * 1024;
            let count = usize::try_from(n).map_err(|_| "repeat count is too large".to_string())?;
            if s.len().checked_mul(count).is_none_or(|b| b > MAX_STR_BYTES) {
                return Err("repeat result exceeds maximum string size".to_string());
            }
            Ok(Value::Str(s.repeat(count).into()))
        }
        "ltrim" => {
            want(name, args, 0)?;
            Ok(Value::Str(s.trim_start().into()))
        }
        "rtrim" => {
            want(name, args, 0)?;
            Ok(Value::Str(s.trim_end().into()))
        }
        "reverse" => {
            want(name, args, 0)?;
            Ok(Value::Str(s.chars().rev().collect::<String>().into()))
        }
        "chars" => {
            want(name, args, 0)?;
            Ok(make_list(s.chars().map(|c| Value::Str(c.to_string().into())).collect()))
        }
        "pad_left" | "pad_right" => {
            if args.len() != 2 {
                return Err(format!(
                    "{} expects a width and a pad string, got {} argument(s)",
                    name,
                    args.len()
                ));
            }
            let width_i64 = want_int(&args[0], &format!("string.{}", name))?;
            if width_i64 < 0 {
                return Err(format!("{} width must be non-negative", name));
            }
            const MAX_PAD_CHARS: usize = 16 * 1024 * 1024;
            let width = usize::try_from(width_i64)
                .ok()
                .filter(|&w| w <= MAX_PAD_CHARS)
                .ok_or_else(|| format!("{} width exceeds maximum string size", name))?;
            let pad = want_str(&args[1], &format!("string.{}", name))?;
            if pad.is_empty() {
                return Err(format!("{}.pad string must not be empty", name));
            }
            let len = s.chars().count();
            if len >= width {
                return Ok(Value::Str(s.into()));
            }
            let fill: String = pad.chars().cycle().take(width - len).collect();
            Ok(Value::Str(if name == "pad_left" {
                format!("{}{}", fill, s)
            } else {
                format!("{}{}", s, fill)
            }
            .into()))
        }
        "format" => {
            // `format("a={} b={}", x, y)` - `{}` placeholders are filled left
            // to right with display forms; `{{`/`}}` escape braces. A
            // placeholder may also carry a precision, `"{:.2}"`, which renders
            // its number with exactly that many fractional digits.
            let mut out = String::new();
            let mut it = args.iter();
            let mut chars = s.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '{' if chars.peek() == Some(&'{') => {
                        chars.next();
                        out.push('{');
                    }
                    '}' if chars.peek() == Some(&'}') => {
                        chars.next();
                        out.push('}');
                    }
                    '{' => {
                        let mut spec = String::new();
                        let mut closed = false;
                        for c2 in chars.by_ref() {
                            if c2 == '}' {
                                closed = true;
                                break;
                            }
                            // ':' and '.' are only meaningful as the start of
                            // a `:.N` precision; anything else still has to be
                            // a literal name character.
                            if !c2.is_ascii_alphanumeric()
                                && c2 != '_'
                                && !(c2 == ':' && spec.is_empty())
                                && !(c2 == '.' && spec.starts_with(':'))
                            {
                                return Err(
                                    "format: placeholders must be '{}' or escape as '{{'".to_string()
                                );
                            }
                            spec.push(c2);
                        }
                        if !closed {
                            return Err("format: unclosed '{' placeholder".to_string());
                        }
                        let v = match it.next() {
                            Some(v) => v,
                            None => {
                                return Err(
                                    "format: more placeholders than arguments".to_string()
                                )
                            }
                        };
                        out.push_str(&format_one_placeholder(&spec, v)?);
                    }
                    '}' => {
                        return Err("format: unexpected '}' (escape as '}}')".to_string());
                    }
                    other => out.push(other),
                }
            }
            if it.next().is_some() {
                return Err("format: more arguments than placeholders".to_string());
            }
            Ok(Value::Str(out.into()))
        }
        _ => Err(unknown(name, "string")),
    }
}

/// Highest precision `{:.N}` accepts. Sixteen digits is past the point where a
/// `f64` carries information, and it keeps a typo like `{:.99}` a diagnostic.
const MAX_FORMAT_PRECISION: usize = 15;

/// Render one `format` placeholder. `spec` is the text between the braces, so
/// `{}` gives `""`, `{:.2}` gives `":.2"`, and `{name}` gives `"name"` (a name
/// is accepted for symmetry with `{}` and still fills positionally).
fn format_one_placeholder(spec: &str, v: &Value) -> Result<String, String> {
    match spec.strip_prefix(":.") {
        Some(digits) => {
            let n: usize = digits.parse().map_err(|_| {
                format!(
                    "format: ':.{}' is not a precision (expected ':.0' through ':.{}')",
                    digits, MAX_FORMAT_PRECISION
                )
            })?;
            if n > MAX_FORMAT_PRECISION {
                return Err(format!(
                    "format: precision ':.{}' is above the maximum ':.{}'",
                    n, MAX_FORMAT_PRECISION
                ));
            }
            // An int is widened so the precision applies to it too: the whole
            // point of a fixed precision is a stable number of decimals.
            match v {
                Value::Int(i) => Ok(format!("{:.*}", n, *i as f64)),
                Value::Float(f) => Ok(format!("{:.*}", n, f)),
                other => Err(format!(
                    "format: ':.{}' expects a number, found {}",
                    n,
                    type_name(other)
                )),
            }
        }
        // Anything else keeps the historical behavior (display form), except
        // that a stray ':' or '.' is now reported instead of being ignored.
        None if spec.contains(':') || spec.contains('.') => Err(format!(
            "format: '{{{}}}' is not a valid placeholder (expected '{{}}' or '{{:.N}}')",
            spec
        )),
        None => Ok(crate::value::display_value(v).to_string()),
    }
}

// -------------------------------------------------- option/result/task/etc.

fn option_method(o: Option<&Value>, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "is_some" => {
            want(name, args, 0)?;
            Ok(Value::Bool(o.is_some()))
        }
        "is_none" => {
            want(name, args, 0)?;
            Ok(Value::Bool(o.is_none()))
        }
        "unwrap" => {
            want(name, args, 0)?;
            match o {
                Some(v) => Ok(v.clone()),
                None => Err("unwrap on none".to_string()),
            }
        }
        "unwrap_or" => {
            want(name, args, 1)?;
            Ok(match o {
                Some(v) => v.clone(),
                None => args[0].clone(),
            })
        }
        _ => Err(unknown(name, "option")),
    }
}

fn result_method(r: Result<&Value, &Value>, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "is_ok" => {
            want(name, args, 0)?;
            Ok(Value::Bool(r.is_ok()))
        }
        "is_err" => {
            want(name, args, 0)?;
            Ok(Value::Bool(r.is_err()))
        }
        "unwrap" => {
            want(name, args, 0)?;
            match r {
                Ok(v) => Ok(v.clone()),
                Err(e) => Err(format!("unwrap on err({})", crate::value::display_value(e))),
            }
        }
        "unwrap_err" => {
            want(name, args, 0)?;
            match r {
                Err(e) => Ok(e.clone()),
                Ok(v) => Err(format!(
                    "unwrap_err on ok({})",
                    crate::value::display_value(v)
                )),
            }
        }
        "unwrap_or" => {
            want(name, args, 1)?;
            Ok(match r {
                Ok(v) => v.clone(),
                Err(_) => args[0].clone(),
            })
        }
        _ => Err(unknown(name, "result")),
    }
}

fn task_method(t: &crate::sched::TaskHandle, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "is_done" => {
            want(name, args, 0)?;
            Ok(Value::Bool(t.is_done()))
        }
        "status" => {
            want(name, args, 0)?;
            Ok(Value::Str(t.status().into()))
        }
        "join" => {
            want(name, args, 0)?;
            // Same contract as `await`: returns the task's value; a failed
            // task raises its error to the caller.
            t.join()
        }
        _ => Err(unknown(name, "task")),
    }
}

fn channel_method(c: &Arc<ChannelData>, name: &str, args: Vec<Value>) -> Result<Value, String> {
    match name {
        "send" => {
            want(name, &args, 1)?;
            c.send(args.into_iter().next().unwrap());
            Ok(Value::Null)
        }
        "recv" => {
            want(name, &args, 0)?;
            Ok(c.recv())
        }
        "try_recv" => {
            want(name, &args, 0)?;
            Ok(match c.try_recv() {
                Some(v) => Value::Option(Some(Box::new(v))),
                None => Value::Option(None),
            })
        }
        "len" => {
            want(name, &args, 0)?;
            Ok(Value::Int(c.len() as i64))
        }
        "is_empty" => {
            want(name, &args, 0)?;
            Ok(Value::Bool(c.is_empty()))
        }
        _ => Err(unknown(name, "channel")),
    }
}

fn mutex_method(m: &MutexData, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "lock" => {
            want(name, args, 0)?;
            Ok(m.lock())
        }
        "unlock" => {
            want(name, args, 0)?;
            m.unlock();
            Ok(Value::Null)
        }
        _ => Err(unknown(name, "mutex")),
    }
}

// -------------------------------------------------------------- indexing

/// `obj[idx]` reads. Supported on list, vec, tuple, string (char), matrix
/// (row-major linear index), hash_table, and map (as `get`).
pub fn index_get(obj: &Value, idx: &Value) -> Result<Value, String> {
    match obj {
        Value::List(d) | Value::VecDyn(d) => {
            let d = d.lock().unwrap();
            let i = want_index(idx, "index")?;
            match d.items.get(i) {
                Some(v) => Ok(v.clone()),
                None => Err(format!(
                    "index {} out of bounds for length {}",
                    i,
                    d.items.len()
                )),
            }
        }
        Value::Tuple(t) => {
            let t = t.lock().unwrap();
            let i = want_index(idx, "index")?;
            match t.get(i) {
                Some(v) => Ok(v.clone()),
                None => Err(format!(
                    "index {} out of bounds for tuple of arity {}",
                    i,
                    t.len()
                )),
            }
        }
        Value::Str(s) => {
            let i = want_index(idx, "index")?;
            match s.chars().nth(i) {
                Some(c) => Ok(Value::Str(c.to_string().into())),
                None => Err(format!(
                    "index {} out of bounds for length {}",
                    i,
                    s.chars().count()
                )),
            }
        }
        Value::Matrix(m) => {
            let m = m.lock().unwrap();
            let i = want_index(idx, "index")?;
            match m.data.get(i) {
                Some(v) => Ok(Value::Float(*v)),
                None => Err(format!(
                    "index {} out of bounds for {}x{} matrix ({} cells)",
                    i,
                    m.rows,
                    m.cols,
                    m.rows * m.cols
                )),
            }
        }
        Value::Vector(v) => {
            let v = v.lock().unwrap();
            let i = misc::resolve_vector_index(idx, v.data.len(), "index")?;
            Ok(Value::Float(v.data[i]))
        }
        Value::Bytes(b) => {
            let b = b.lock().unwrap();
            let i = misc::resolve_bytes_index(idx, b.len(), "bytes index")?;
            Ok(Value::Int(b[i] as i64))
        }
        Value::HashTable(h) => {
            let h = h.lock().unwrap();
            let k = as_map_key(idx)?;
            h.get(&k)
        }
        Value::Map(d) => {
            let d = d.lock().unwrap();
            let k = as_map_key(idx)?;
            match d.find(&k) {
                Some(i) => Ok(d.entries[i].1.clone()),
                None => Err(format!("missing key {}", k.display())),
            }
        }
        other => Err(format!("{} does not support indexing", type_name(other))),
    }
}

/// `obj[idx] = v` writes. Supported on list, vec, matrix, and hash_table.
/// Tuples reject assignment on purpose.
pub fn index_set(obj: &Value, idx: &Value, v: Value) -> Result<(), String> {
    match obj {
        Value::List(d) | Value::VecDyn(d) => {
            let mut d = d.lock().unwrap();
            let i = want_index(idx, "index")?;
            match d.items.get_mut(i) {
                Some(slot) => {
                    *slot = v;
                    Ok(())
                }
                None => Err(format!(
                    "index {} out of bounds for length {}",
                    i,
                    d.items.len()
                )),
            }
        }
        Value::Matrix(m) => {
            let mut m = m.lock().unwrap();
            let i = want_index(idx, "index")?;
            match m.data.get_mut(i) {
                Some(slot) => {
                    let n = want_num(&v, "matrix index assignment")?;
                    *slot = n;
                    Ok(())
                }
                None => Err(format!(
                    "index {} out of bounds for {}x{} matrix ({} cells)",
                    i,
                    m.rows,
                    m.cols,
                    m.rows * m.cols
                )),
            }
        }
        Value::Vector(vec_ref) => {
            let mut vec_guard = vec_ref.lock().unwrap();
            let i = misc::resolve_vector_index(idx, vec_guard.data.len(), "index")?;
            let n = want_num(&v, "vector index assignment")?;
            vec_guard.data[i] = n;
            Ok(())
        }
        Value::Bytes(b) => {
            let mut b = b.lock().unwrap();
            let i = misc::resolve_bytes_index(idx, b.len(), "bytes index")?;
            let val = want_int(&v, "bytes index assignment")?;
            if !(0..=255).contains(&val) {
                return Err(format!("byte value must be between 0 and 255, got {}", val));
            }
            b[i] = val as u8;
            Ok(())
        }
        Value::HashTable(h) => {
            let mut h = h.lock().unwrap();
            let k = as_map_key(idx)?;
            h.set(k, v)?;
            Ok(())
        }
        Value::Map(d) => {
            // `m[k] = v` is documented (docs/DATA_STRUCTURES.md) as an alias
            // for `m.set(k, v)`; MapData::set keeps insertion order and
            // validates the key type itself.
            d.lock().unwrap().method("set", &[idx.clone(), v])?;
            Ok(())
        }
        Value::Tuple(_) => Err("tuple is immutable: index assignment fails".to_string()),
        other => Err(format!(
            "{} does not support index assignment",
            type_name(other)
        )),
    }
}

// -------------------------------------------------------------- iteration

/// Values produced by `for x in <expr>`, in each structure's natural order.
///
/// Lock discipline (audit F3/p01): every arm that deep-clones items first
/// snapshots the contents under the container's lock and releases it before
/// any `deep_clone` recursion. An item can be (or reach, through a cycle) the
/// container itself, and `deep_clone` re-locks container mutexes — holding a
/// lock across the recursion deadlocked the loop forever.
pub fn iter_values(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::List(d) | Value::VecDyn(d) => {
            let items = d.lock().unwrap().items.clone();
            Ok(items.iter().map(deep_clone).collect())
        }
        Value::Stack(d) => {
            let items = d.lock().unwrap().items.clone();
            Ok(items.iter().rev().map(deep_clone).collect())
        }
        Value::Queue(d) => {
            let items = d.lock().unwrap().items.clone();
            Ok(items.iter().map(deep_clone).collect())
        }
        Value::Deque(d) => {
            let items = d.lock().unwrap().items.clone();
            Ok(items.iter().map(deep_clone).collect())
        }
        Value::Set(d) => {
            let items = d.lock().unwrap().items.clone();
            Ok(items.iter().map(deep_clone).collect())
        }
        Value::Ring(d) => {
            let items = d.lock().unwrap().items.clone();
            Ok(items.iter().map(deep_clone).collect())
        }
        Value::Tuple(t) => {
            let items = t.lock().unwrap().clone();
            Ok(items.iter().map(deep_clone).collect())
        }
        Value::Map(d) => Ok(d
            .lock()
            .unwrap()
            .entries
            .iter()
            .map(|(k, _)| key_to_value(k))
            .collect()),
        Value::Bst(d) => {
            let items = d.lock().unwrap().inorder();
            Ok(items.iter().map(deep_clone).collect())
        }
        Value::BinTree(d) => {
            let items = d.lock().unwrap().level_order();
            Ok(items.iter().map(deep_clone).collect())
        }
        Value::Heap(d) => {
            let items = d.lock().unwrap().items.clone();
            Ok(items.iter().map(deep_clone).collect())
        }
        Value::Graph(d) => {
            let nodes = d.lock().unwrap().nodes.clone();
            Ok(nodes.iter().map(deep_clone).collect())
        }
        Value::HashTable(d) => {
            let d = d.lock().unwrap();
            let mut out = Vec::new();
            for b in &d.buckets {
                for (k, _) in b {
                    out.push(key_to_value(k));
                }
            }
            Ok(out)
        }
        Value::Trie(d) => {
            let words = d.lock().unwrap().all_words();
            Ok(words.into_iter().map(|w| Value::Str(w.into())).collect())
        }
        Value::Vector(d) => {
            let items = d.lock().unwrap().data.clone();
            Ok(items.into_iter().map(Value::Float).collect())
        }
        Value::Bytes(d) => {
            let items = d.lock().unwrap().clone();
            Ok(items.into_iter().map(|b| Value::Int(b as i64)).collect())
        }
        Value::Bag(d) => {
            let counts = d.lock().unwrap().counts.clone();
            let mut out = Vec::new();
            for (v, n) in &counts {
                for _ in 0..*n {
                    out.push(deep_clone(v));
                }
            }
            Ok(out)
        }
        Value::Str(s) => Ok(s.chars().map(|c| Value::Str(c.to_string().into())).collect()),
        Value::LinkedList(d) | Value::SinglyList(d) | Value::DoublyList(d) => {
            // to_vec() walks the nodes under the list lock and returns shallow
            // (Arc-cheap) value clones; the guard drops with the statement, so
            // the deep clones below re-lock nothing that is still held.
            let vals = d.lock().unwrap().to_vec();
            Ok(vals.iter().map(deep_clone).collect())
        }
        other => Err(format!(
            "{} is not iterable",
            crate::value::type_name(other)
        )),
    }
}

/// Length for the global `len()` function.
pub fn len_of(v: &Value) -> Result<usize, String> {
    match v {
        Value::List(d) | Value::VecDyn(d) => Ok(d.lock().unwrap().items.len()),
        Value::Stack(d) => Ok(d.lock().unwrap().items.len()),
        Value::Queue(d) => Ok(d.lock().unwrap().items.len()),
        Value::Deque(d) => Ok(d.lock().unwrap().items.len()),
        Value::Map(d) => Ok(d.lock().unwrap().entries.len()),
        Value::Set(d) => Ok(d.lock().unwrap().items.len()),
        Value::Tuple(t) => Ok(t.lock().unwrap().len()),
        Value::Bst(d) => Ok(d.lock().unwrap().len),
        Value::BinTree(d) => Ok(d.lock().unwrap().len),
        Value::Heap(d) => Ok(d.lock().unwrap().items.len()),
        Value::PrioQueue(d) => Ok(d.lock().unwrap().items.len()),
        Value::Graph(d) => Ok(d.lock().unwrap().nodes.len()),
        Value::HashTable(d) => Ok(d.lock().unwrap().size),
        Value::Trie(d) => Ok(d.lock().unwrap().count),
        Value::Ring(d) => Ok(d.lock().unwrap().items.len()),
        Value::Matrix(d) => {
            let d = d.lock().unwrap();
            Ok(d.rows * d.cols)
        }
        Value::Vector(d) => Ok(d.lock().unwrap().data.len()),
        Value::Bytes(d) => Ok(d.lock().unwrap().len()),
        Value::Bag(d) => Ok(d.lock().unwrap().counts.iter().map(|(_, n)| *n).sum()),
        Value::LinkedList(d) | Value::SinglyList(d) | Value::DoublyList(d) => {
            Ok(d.lock().unwrap().len)
        }
        Value::Str(s) => Ok(s.chars().count()),
        Value::Option(o) => Ok(if o.is_some() { 1 } else { 0 }),
        other => Err(format!("{} has no length", crate::value::type_name(other))),
    }
}
