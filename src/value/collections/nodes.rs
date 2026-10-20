//! Honest node structures: linked lists with real nodes and pointers, a real
//! binary search tree with real child links, and a plain binary tree filled
//! level-order. None of these are `Vec` wrappers.

use std::sync::{Arc, Mutex};

use super::seq::{make_list, unknown, want, want_index};
use super::{deep_clone, new_ref, values_equal, Value};

// ------------------------------------------------------------- linked lists

pub type NodeRef = Arc<Mutex<LLNode>>;

#[derive(Debug)]
pub struct LLNode {
    pub value: Value,
    pub next: Option<NodeRef>,
    pub prev: Option<NodeRef>,
}

/// One node layout for all three list kinds, with a flag for `prev` links:
/// - `linked_list`: `tail` is maintained, so `push_back` is O(1) and
///   `pop_back` is O(n) (there is no `prev`).
/// - `singly_linked_list`: no tail pointer is maintained, so `push_back` and
///   `pop_back` are O(n) and `push_front`/`pop_front` are O(1).
/// - `doubly_linked_list`: `prev` links, O(1) at both ends.
#[derive(Debug, Default)]
pub struct ListData {
    pub head: Option<NodeRef>,
    pub tail: Option<NodeRef>,
    pub len: usize,
    pub doubly: bool,
    /// linked_list maintains a tail pointer; the singly list does not.
    pub keep_tail: bool,
}

impl ListData {
    pub fn linked() -> Self {
        ListData {
            head: None,
            tail: None,
            len: 0,
            doubly: false,
            keep_tail: true,
        }
    }
    pub fn singly() -> Self {
        ListData {
            head: None,
            tail: None,
            len: 0,
            doubly: false,
            keep_tail: false,
        }
    }
    pub fn doubly() -> Self {
        ListData {
            head: None,
            tail: None,
            len: 0,
            doubly: true,
            keep_tail: true,
        }
    }

    fn kind_name(&self) -> &'static str {
        if self.doubly {
            "doubly_linked_list"
        } else if self.keep_tail {
            "linked_list"
        } else {
            "singly_linked_list"
        }
    }

    pub fn push_back(&mut self, v: Value) {
        let node: NodeRef = Arc::new(Mutex::new(LLNode {
            value: v,
            next: None,
            prev: None,
        }));
        if self.head.is_none() {
            self.head = Some(node.clone());
            if self.keep_tail {
                self.tail = Some(node);
            }
            self.len += 1;
            return;
        }
        if let Some(t) = self.tail.clone() {
            // O(1): append at the maintained tail.
            t.lock().unwrap().next = Some(node.clone());
            if self.doubly {
                node.lock().unwrap().prev = Some(t);
            }
            self.tail = Some(node);
            self.len += 1;
            return;
        }
        // singly, no tail: walk to the last node. O(n).
        let mut cur = self.head.clone().unwrap();
        loop {
            let next = cur.lock().unwrap().next.clone();
            match next {
                Some(n) => cur = n,
                None => break,
            }
        }
        cur.lock().unwrap().next = Some(node.clone());
        if self.doubly {
            node.lock().unwrap().prev = Some(cur);
        }
        self.len += 1;
    }

    fn push_front(&mut self, v: Value) {
        let node: NodeRef = Arc::new(Mutex::new(LLNode {
            value: v,
            next: None,
            prev: None,
        }));
        match self.head.clone() {
            Some(h) => {
                node.lock().unwrap().next = Some(h.clone());
                if self.doubly {
                    h.lock().unwrap().prev = Some(node.clone());
                }
                self.head = Some(node);
            }
            None => {
                self.head = Some(node.clone());
                if self.keep_tail {
                    self.tail = Some(node);
                }
            }
        }
        self.len += 1;
    }

    fn pop_front(&mut self) -> Result<Value, String> {
        let h = match self.head.clone() {
            Some(h) => h,
            None => return Err("pop_front on an empty list".to_string()),
        };
        let next = {
            let hb = h.lock().unwrap();
            hb.next.clone()
        };
        match &next {
            Some(n) => {
                if self.doubly {
                    n.lock().unwrap().prev = None;
                }
            }
            None => {
                self.tail = None;
            }
        }
        self.head = next;
        self.len -= 1;
        let v = std::mem::replace(&mut h.lock().unwrap().value, Value::Null);
        Ok(v)
    }

    fn pop_back(&mut self) -> Result<Value, String> {
        if self.head.is_none() {
            return Err("pop_back on an empty list".to_string());
        }
        if self.doubly {
            // O(1) via prev links.
            let t = self.tail.clone().unwrap();
            let prev = t.lock().unwrap().prev.clone();
            match &prev {
                Some(p) => {
                    p.lock().unwrap().next = None;
                    self.tail = Some(p.clone());
                }
                None => {
                    self.head = None;
                    self.tail = None;
                }
            }
            self.len -= 1;
            let v = std::mem::replace(&mut t.lock().unwrap().value, Value::Null);
            return Ok(v);
        }
        // No prev links: walk to the second-to-last node. O(n).
        let mut cur = self.head.clone().unwrap();
        loop {
            let next = cur.lock().unwrap().next.clone();
            match next {
                Some(n) => {
                    let has_next = n.lock().unwrap().next.is_some();
                    if has_next {
                        cur = n;
                    } else {
                        // n is the last node
                        cur.lock().unwrap().next = None;
                        self.tail = if self.keep_tail { Some(cur) } else { None };
                        self.len -= 1;
                        let v = std::mem::replace(&mut n.lock().unwrap().value, Value::Null);
                        return Ok(v);
                    }
                }
                None => {
                    // cur is the only node
                    self.head = None;
                    if self.keep_tail {
                        self.tail = None;
                    }
                    self.len -= 1;
                    let v = std::mem::replace(&mut cur.lock().unwrap().value, Value::Null);
                    return Ok(v);
                }
            }
        }
    }

    fn peek_front(&self) -> Result<Value, String> {
        match &self.head {
            Some(h) => Ok(h.lock().unwrap().value.clone()),
            None => Err("peek_front on an empty list".to_string()),
        }
    }

    fn peek_back(&self) -> Result<Value, String> {
        let last = if self.keep_tail {
            self.tail.clone()
        } else {
            // walk
            let mut cur = self.head.clone();
            let mut last = None;
            while let Some(n) = cur {
                last = Some(n.clone());
                cur = n.lock().unwrap().next.clone();
            }
            last
        };
        match last {
            Some(n) => Ok(n.lock().unwrap().value.clone()),
            None => Err("peek_back on an empty list".to_string()),
        }
    }

    fn get(&self, i: usize) -> Result<Value, String> {
        let mut cur = self.head.clone();
        let mut n = 0usize;
        while let Some(node) = cur {
            if n == i {
                return Ok(node.lock().unwrap().value.clone());
            }
            n += 1;
            cur = node.lock().unwrap().next.clone();
        }
        Err(format!("index {} out of bounds for length {}", i, self.len))
    }

    fn contains(&self, v: &Value) -> bool {
        let mut cur = self.head.clone();
        while let Some(node) = cur {
            if values_equal(&node.lock().unwrap().value, v) {
                return true;
            }
            cur = node.lock().unwrap().next.clone();
        }
        false
    }

    fn remove_value(&mut self, v: &Value) -> Result<Value, String> {
        // find the node and its predecessor
        let mut prev: Option<NodeRef> = None;
        let mut cur = self.head.clone();
        while let Some(node) = cur {
            if values_equal(&node.lock().unwrap().value, v) {
                let next = node.lock().unwrap().next.clone();
                match &prev {
                    Some(p) => p.lock().unwrap().next = next.clone(),
                    None => self.head = next.clone(),
                }
                if let Some(n) = &next {
                    if self.doubly {
                        n.lock().unwrap().prev = prev.clone();
                    }
                } else if self.keep_tail {
                    self.tail = prev.clone();
                }
                self.len -= 1;
                let out = std::mem::replace(&mut node.lock().unwrap().value, Value::Null);
                return Ok(out);
            }
            prev = Some(node.clone());
            cur = node.lock().unwrap().next.clone();
        }
        Err("value not in list".to_string())
    }

    /// Shallow snapshot of the node values, head first. Callers that recurse
    /// into the values (deep_clone) must drop the list's lock first — a node
    /// value can be the list itself (audit F3/p06).
    pub fn to_vec(&self) -> Vec<Value> {
        let mut out = Vec::new();
        let mut cur = self.head.clone();
        while let Some(node) = cur {
            out.push(node.lock().unwrap().value.clone());
            cur = node.lock().unwrap().next.clone();
        }
        out
    }

    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.len as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.len == 0))
            }
            "clear" => {
                want(name, args, 0)?;
                self.clear_nodes();
                Ok(Value::Null)
            }
            "push_back" => {
                want(name, args, 1)?;
                self.push_back(args[0].clone());
                Ok(Value::Null)
            }
            "push_front" => {
                want(name, args, 1)?;
                self.push_front(args[0].clone());
                Ok(Value::Null)
            }
            "pop_front" => {
                want(name, args, 0)?;
                self.pop_front()
            }
            "pop_back" => {
                want(name, args, 0)?;
                self.pop_back()
            }
            "peek_front" => {
                want(name, args, 0)?;
                self.peek_front()
            }
            "peek_back" => {
                want(name, args, 0)?;
                self.peek_back()
            }
            "get" => {
                want(name, args, 1)?;
                let i = want_index(&args[0], "get")?;
                self.get(i)
            }
            "contains" => {
                want(name, args, 1)?;
                Ok(Value::Bool(self.contains(&args[0])))
            }
            "remove" => {
                want(name, args, 1)?;
                self.remove_value(&args[0])
            }
            "to_list" => {
                want(name, args, 0)?;
                let vals = self.to_vec();
                Ok(make_list(vals.iter().map(deep_clone).collect()))
            }
            "clone" => {
                want(name, args, 0)?;
                let items: Vec<Value> = self.to_vec().iter().map(deep_clone).collect();
                let mut nd = if self.doubly {
                    ListData::doubly()
                } else if self.keep_tail {
                    ListData::linked()
                } else {
                    ListData::singly()
                };
                for it in items {
                    nd.push_back(it);
                }
                Ok(new_list_value(nd, self.doubly, self.keep_tail))
            }
            _ => Err(unknown(name, self.kind_name())),
        }
    }

    /// Break all internal `prev`/`next` node links and clear node values so
    /// doubly-linked lists never leak their `LLNode` allocations on drop or GC.
    pub fn clear_nodes(&mut self) {
        let mut cur = self.head.take();
        self.tail = None;
        self.len = 0;
        while let Some(node) = cur {
            if let Ok(mut b) = node.lock() {
                b.prev = None;
                b.value = Value::Null;
                cur = b.next.take();
            } else {
                break;
            }
        }
    }
}

impl Drop for ListData {
    fn drop(&mut self) {
        self.clear_nodes();
    }
}

pub fn new_list_value(mut nd: ListData, doubly: bool, keep_tail: bool) -> Value {
    nd.doubly = doubly;
    nd.keep_tail = keep_tail;
    let d = new_ref(nd);
    if doubly {
        Value::DoublyList(d)
    } else if keep_tail {
        Value::LinkedList(d)
    } else {
        Value::SinglyList(d)
    }
}

// ---------------------------------------------------------------------- BST

pub type BstRef = Arc<Mutex<BstNode>>;

#[derive(Debug)]
pub struct BstNode {
    pub value: Value,
    pub left: Option<BstRef>,
    pub right: Option<BstRef>,
}

/// Unbalanced binary search tree. Inorder traversal is ascending.
#[derive(Debug, Default)]
pub struct BstData {
    pub root: Option<BstRef>,
    pub len: usize,
}

impl BstData {
    pub fn new() -> Self {
        BstData { root: None, len: 0 }
    }

    fn cmp(a: &Value, b: &Value) -> Result<std::cmp::Ordering, String> {
        super::seq::value_cmp(a, b).ok_or_else(|| {
            format!(
                "bst values must be numbers or strings; {} is not comparable",
                super::type_name(a)
            )
        })
    }

    pub fn insert(&mut self, v: Value) -> Result<Value, String> {
        Self::cmp(&v, &v)?;
        let node: BstRef = Arc::new(Mutex::new(BstNode {
            value: v,
            left: None,
            right: None,
        }));
        let cmp_target = node.lock().unwrap().value.clone();
        if self.root.is_none() {
            self.root = Some(node);
            self.len += 1;
            return Ok(Value::Null);
        }
        let mut cur = self.root.clone().unwrap();
        loop {
            let order = Self::cmp(&cur.lock().unwrap().value, &cmp_target)?;
            if order == std::cmp::Ordering::Equal {
                return Err("duplicate value in bst".to_string());
            }
            let go_left = order == std::cmp::Ordering::Greater;
            let child = if go_left {
                cur.lock().unwrap().left.clone()
            } else {
                cur.lock().unwrap().right.clone()
            };
            match child {
                Some(c) => cur = c,
                None => {
                    if go_left {
                        cur.lock().unwrap().left = Some(node);
                    } else {
                        cur.lock().unwrap().right = Some(node);
                    }
                    self.len += 1;
                    return Ok(Value::Null);
                }
            }
        }
    }

    pub fn contains(&self, v: &Value) -> Result<bool, String> {
        let mut cur = self.root.clone();
        while let Some(node) = cur {
            let order = Self::cmp(&node.lock().unwrap().value, v)?;
            match order {
                std::cmp::Ordering::Equal => return Ok(true),
                std::cmp::Ordering::Greater => cur = node.lock().unwrap().left.clone(),
                std::cmp::Ordering::Less => cur = node.lock().unwrap().right.clone(),
            }
        }
        Ok(false)
    }

    /// Deletion handles all three cases: leaf, one child, two children
    /// (successor replacement).
    pub fn remove(&mut self, v: &Value) -> Result<Value, String> {
        let found = Self::remove_node(&mut self.root, v)?;
        if found {
            self.len -= 1;
        }
        Ok(Value::Null)
    }

    /// Detach the minimum node of a subtree. Returns the detached node and the
    /// new subtree root (None when the subtree became empty). Never holds a
    /// node lock across the recursion.
    fn remove_min(node: BstRef) -> (BstRef, Option<BstRef>) {
        let left = node.lock().unwrap().left.clone();
        match left {
            None => {
                let right = node.lock().unwrap().right.clone();
                (node, right)
            }
            Some(l) => {
                let (min, new_left) = Self::remove_min(l);
                node.lock().unwrap().left = new_left;
                (min, Some(node))
            }
        }
    }

    fn remove_node(slot: &mut Option<BstRef>, v: &Value) -> Result<bool, String> {
        let node = match slot {
            Some(n) => n.clone(),
            None => return Ok(false),
        };
        let order = Self::cmp(&node.lock().unwrap().value, v)?;
        match order {
            std::cmp::Ordering::Equal => {
                let has_left = node.lock().unwrap().left.is_some();
                let has_right = node.lock().unwrap().right.is_some();
                match (has_left, has_right) {
                    (false, false) => *slot = None,
                    (true, false) => *slot = node.lock().unwrap().left.clone(),
                    (false, true) => *slot = node.lock().unwrap().right.clone(),
                    (true, true) => {
                        // replace with the inorder successor
                        let right = node.lock().unwrap().right.clone().unwrap();
                        let (succ, new_right) = Self::remove_min(right);
                        let succ_left = node.lock().unwrap().left.clone();
                        succ.lock().unwrap().left = succ_left;
                        succ.lock().unwrap().right = new_right;
                        *slot = Some(succ);
                    }
                }
                Ok(true)
            }
            std::cmp::Ordering::Greater => {
                let mut left = node.lock().unwrap().left.clone();
                let found = Self::remove_node(&mut left, v)?;
                node.lock().unwrap().left = left;
                Ok(found)
            }
            std::cmp::Ordering::Less => {
                let mut right = node.lock().unwrap().right.clone();
                let found = Self::remove_node(&mut right, v)?;
                node.lock().unwrap().right = right;
                Ok(found)
            }
        }
    }

    fn walk(&self, order: u8, out: &mut Vec<Value>) {
        fn go(node: &BstRef, order: u8, out: &mut Vec<Value>) {
            let (l, r, v) = {
                let b = node.lock().unwrap();
                (b.left.clone(), b.right.clone(), b.value.clone())
            };
            if order == 0 {
                if let Some(l) = &l {
                    go(l, order, out);
                }
                out.push(v);
                if let Some(r) = &r {
                    go(r, order, out);
                }
            } else if order == 1 {
                out.push(v);
                if let Some(l) = &l {
                    go(l, order, out);
                }
                if let Some(r) = &r {
                    go(r, order, out);
                }
            } else {
                if let Some(l) = &l {
                    go(l, order, out);
                }
                if let Some(r) = &r {
                    go(r, order, out);
                }
                out.push(v);
            }
        }
        if let Some(root) = &self.root {
            go(root, order, out);
        }
    }

    pub fn inorder(&self) -> Vec<Value> {
        let mut out = Vec::new();
        self.walk(0, &mut out);
        out
    }
    pub fn preorder(&self) -> Vec<Value> {
        let mut out = Vec::new();
        self.walk(1, &mut out);
        out
    }
    pub fn postorder(&self) -> Vec<Value> {
        let mut out = Vec::new();
        self.walk(2, &mut out);
        out
    }

    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.len as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.len == 0))
            }
            "clear" => {
                want(name, args, 0)?;
                self.root = None;
                self.len = 0;
                Ok(Value::Null)
            }
            "insert" => {
                want(name, args, 1)?;
                self.insert(args[0].clone())
            }
            "contains" => {
                want(name, args, 1)?;
                Ok(Value::Bool(self.contains(&args[0])?))
            }
            "remove" => {
                want(name, args, 1)?;
                if !self.contains(&args[0])? {
                    return Err("value not in bst".to_string());
                }
                self.remove(&args[0])
            }
            "inorder" | "preorder" | "postorder" => {
                want(name, args, 0)?;
                let items = match name {
                    "inorder" => self.inorder(),
                    "preorder" => self.preorder(),
                    _ => self.postorder(),
                };
                Ok(make_list(items.iter().map(deep_clone).collect()))
            }
            "root" => {
                want(name, args, 0)?;
                match &self.root {
                    Some(r) => Ok(Value::Option(Some(Box::new(
                        r.lock().unwrap().value.clone(),
                    )))),
                    None => Ok(Value::Option(None)),
                }
            }
            "clone" => {
                want(name, args, 0)?;
                let mut nd = BstData::new();
                for v in self.inorder() {
                    nd.insert(deep_clone(&v))?;
                }
                Ok(Value::Bst(new_ref(nd)))
            }
            _ => Err(unknown(name, "bst")),
        }
    }
}

// -------------------------------------------------------------- binary tree

pub type BtRef = Arc<Mutex<BtNode>>;

#[derive(Debug)]
pub struct BtNode {
    pub value: Value,
    pub left: Option<BtRef>,
    pub right: Option<BtRef>,
}

/// A plain binary tree: not a search tree. `insert` fills the next open slot
/// in level order, so traversals exercise real node links.
#[derive(Debug, Default)]
pub struct BinTreeData {
    pub root: Option<BtRef>,
    /// Nodes with a free child slot, in level order.
    pub open: Vec<BtRef>,
    pub len: usize,
}

impl BinTreeData {
    pub fn new() -> Self {
        BinTreeData {
            root: None,
            open: Vec::new(),
            len: 0,
        }
    }

    pub fn insert(&mut self, v: Value) -> Result<Value, String> {
        let node: BtRef = Arc::new(Mutex::new(BtNode {
            value: v,
            left: None,
            right: None,
        }));
        if self.root.is_none() {
            self.root = Some(node.clone());
            self.open.push(node);
            self.len += 1;
            return Ok(Value::Null);
        }
        // attach to the first open node that still has a free slot
        while let Some(front) = self.open.first().cloned() {
            let mut fb = front.lock().unwrap();
            if fb.left.is_none() {
                fb.left = Some(node.clone());
                self.open.push(node);
                self.len += 1;
                return Ok(Value::Null);
            }
            if fb.right.is_none() {
                fb.right = Some(node.clone());
                self.open.push(node);
                self.len += 1;
                return Ok(Value::Null);
            }
            drop(fb);
            self.open.remove(0);
        }
        Err("binary_tree is full".to_string())
    }

    fn walk(&self, order: u8, out: &mut Vec<Value>) {
        fn go(node: &BtRef, order: u8, out: &mut Vec<Value>) {
            let (l, r, v) = {
                let b = node.lock().unwrap();
                (b.left.clone(), b.right.clone(), b.value.clone())
            };
            if order == 0 {
                if let Some(l) = &l {
                    go(l, order, out);
                }
                out.push(v);
                if let Some(r) = &r {
                    go(r, order, out);
                }
            } else if order == 1 {
                out.push(v);
                if let Some(l) = &l {
                    go(l, order, out);
                }
                if let Some(r) = &r {
                    go(r, order, out);
                }
            } else {
                if let Some(l) = &l {
                    go(l, order, out);
                }
                if let Some(r) = &r {
                    go(r, order, out);
                }
                out.push(v);
            }
        }
        if let Some(root) = &self.root {
            go(root, order, out);
        }
    }

    pub fn level_order(&self) -> Vec<Value> {
        let mut out = Vec::new();
        let mut queue = std::collections::VecDeque::new();
        if let Some(r) = &self.root {
            queue.push_back(r.clone());
        }
        while let Some(n) = queue.pop_front() {
            let (l, r, v) = {
                let b = n.lock().unwrap();
                (b.left.clone(), b.right.clone(), b.value.clone())
            };
            out.push(v);
            if let Some(l) = l {
                queue.push_back(l);
            }
            if let Some(r) = r {
                queue.push_back(r);
            }
        }
        out
    }

    pub fn postorder(&self) -> Vec<Value> {
        let mut out = Vec::new();
        self.walk(2, &mut out);
        out
    }

    /// Inorder traversal of the plain binary tree (left, root, right).
    pub fn inorder(&self) -> Vec<Value> {
        let mut out = Vec::new();
        self.walk(0, &mut out);
        out
    }

    pub fn preorder(&self) -> Vec<Value> {
        let mut out = Vec::new();
        self.walk(1, &mut out);
        out
    }

    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.len as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.len == 0))
            }
            "clear" => {
                want(name, args, 0)?;
                self.root = None;
                self.open.clear();
                self.len = 0;
                Ok(Value::Null)
            }
            "insert" => {
                want(name, args, 1)?;
                self.insert(args[0].clone())
            }
            "inorder" | "preorder" | "postorder" | "level_order" => {
                want(name, args, 0)?;
                let mut out = Vec::new();
                match name {
                    "inorder" => self.walk(0, &mut out),
                    "preorder" => self.walk(1, &mut out),
                    "postorder" => self.walk(2, &mut out),
                    _ => return Ok(make_list(self.level_order())),
                }
                Ok(make_list(out.iter().map(deep_clone).collect()))
            }
            "root" => {
                want(name, args, 0)?;
                match &self.root {
                    Some(r) => Ok(Value::Option(Some(Box::new(
                        r.lock().unwrap().value.clone(),
                    )))),
                    None => Ok(Value::Option(None)),
                }
            }
            "clone" => {
                want(name, args, 0)?;
                let mut nd = BinTreeData::new();
                for v in self.level_order() {
                    nd.insert(deep_clone(&v))?;
                }
                Ok(Value::BinTree(new_ref(nd)))
            }
            _ => Err(unknown(name, "binary_tree")),
        }
    }
}

/// Detached traversals (audit A-7): node values are deep-cloned OUTSIDE the
/// container's Mutex — a node value can be the tree itself, and deep_clone
/// would re-lock the mutex the dispatcher holds. The traversal itself only
/// takes the per-node mutexes (statement-scoped), so it may run under the
/// container lock; the deep clone must not.
pub fn bintree_traverse_detached(
    d: &crate::value::Ref<BinTreeData>,
    name: &str,
) -> Result<Value, String> {
    let vals: Vec<Value> = {
        let t = d.lock().unwrap();
        match name {
            "inorder" => t.inorder(),
            "preorder" => t.preorder(),
            "postorder" => t.postorder(),
            "level_order" => t.level_order(),
            _ => return Err(unknown(name, "binary_tree")),
        }
    };
    Ok(make_list(vals.iter().map(deep_clone).collect()))
}

/// Build a `string` list value (used by trie word output).
pub fn join_words(words: &[String]) -> Value {
    make_list(words.iter().map(|w| Value::Str(w.clone().into())).collect())
}
