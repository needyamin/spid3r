//! Remaining structures: heap, priority_queue (stable ties), graph, hash_table
//! (visible buckets and rehash), trie, matrix, ring_buffer, bag, ref.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use super::nodes::join_words;
use super::seq::{
    make_list, unknown, value_cmp, want, want_index, want_int, want_num, want_str, MapData,
};
use crate::value::Ref;
use super::{deep_clone, key_to_value, new_ref, values_equal, MapKey, Value};

// -------------------------------------------------------------------- heap

#[derive(Debug, Default)]
pub struct HeapData {
    pub items: Vec<Value>,
    /// true = min-heap, false = max-heap
    pub min: bool,
}

impl HeapData {
    pub fn new(min: bool) -> Self {
        HeapData {
            items: Vec::new(),
            min,
        }
    }

    fn less(&self, a: &Value, b: &Value) -> Result<bool, String> {
        match value_cmp(a, b) {
            Some(o) => Ok(if self.min {
                o == std::cmp::Ordering::Less
            } else {
                o == std::cmp::Ordering::Greater
            }),
            None => Err("heap values must be numbers or strings".to_string()),
        }
    }

    fn sift_up(&mut self, mut i: usize) -> Result<(), String> {
        while i > 0 {
            let parent = (i - 1) / 2;
            if self.less(&self.items[i], &self.items[parent])? {
                self.items.swap(i, parent);
                i = parent;
            } else {
                break;
            }
        }
        Ok(())
    }

    fn sift_down(&mut self, mut i: usize) -> Result<(), String> {
        let n = self.items.len();
        loop {
            let l = 2 * i + 1;
            let r = 2 * i + 2;
            let mut best = i;
            if l < n && self.less(&self.items[l], &self.items[best])? {
                best = l;
            }
            if r < n && self.less(&self.items[r], &self.items[best])? {
                best = r;
            }
            if best == i {
                break;
            }
            self.items.swap(i, best);
            i = best;
        }
        Ok(())
    }

    pub fn push(&mut self, v: Value) -> Result<Value, String> {
        self.less(&v, &v)?;
        if let Some(first) = self.items.first() {
            self.less(&v, first)?;
        }
        self.items.push(v);
        let n = self.items.len();
        self.sift_up(n - 1)?;
        Ok(Value::Null)
    }

    pub fn pop(&mut self) -> Result<Value, String> {
        if self.items.is_empty() {
            return Err("pop on an empty heap".to_string());
        }
        let n = self.items.len();
        self.items.swap(0, n - 1);
        let out = self.items.pop().unwrap();
        self.sift_down(0)?;
        Ok(out)
    }

    pub fn peek(&self) -> Result<Value, String> {
        match self.items.first() {
            Some(v) => Ok(v.clone()),
            None => Err("peek on an empty heap".to_string()),
        }
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
            "push" => {
                want(name, args, 1)?;
                self.push(args[0].clone())
            }
            "pop" => {
                want(name, args, 0)?;
                self.pop()
            }
            "peek" => {
                want(name, args, 0)?;
                self.peek()
            }
            "is_min" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.min))
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::Heap(new_ref(HeapData {
                    items: self.items.iter().map(deep_clone).collect(),
                    min: self.min,
                })))
            }
            _ => Err(unknown(name, "heap")),
        }
    }
}

// ----------------------------------------------------------- priority_queue

#[derive(Debug, Clone)]
pub struct PqItem {
    pub value: Value,
    pub prio: f64,
    pub seq: u64,
}

/// Heap-ordered queue that breaks ties by arrival order. Not a rename of
/// `heap`: equal priorities come out in insertion order.
#[derive(Debug, Default)]
pub struct PrioQueueData {
    pub items: Vec<PqItem>,
    pub min: bool,
    pub seq: u64,
}

impl PrioQueueData {
    pub fn new(min: bool) -> Self {
        PrioQueueData {
            items: Vec::new(),
            min,
            seq: 0,
        }
    }

    fn better(&self, a: &PqItem, b: &PqItem) -> bool {
        if self.min {
            a.prio < b.prio || (a.prio == b.prio && a.seq < b.seq)
        } else {
            a.prio > b.prio || (a.prio == b.prio && a.seq < b.seq)
        }
    }

    fn sift_up(&mut self, mut i: usize) {
        while i > 0 {
            let parent = (i - 1) / 2;
            if self.better(&self.items[i].clone(), &self.items[parent].clone()) {
                self.items.swap(i, parent);
                i = parent;
            } else {
                break;
            }
        }
    }

    fn sift_down(&mut self, mut i: usize) {
        let n = self.items.len();
        loop {
            let l = 2 * i + 1;
            let r = 2 * i + 2;
            let mut best = i;
            if l < n && self.better(&self.items[l].clone(), &self.items[best].clone()) {
                best = l;
            }
            if r < n && self.better(&self.items[r].clone(), &self.items[best].clone()) {
                best = r;
            }
            if best == i {
                break;
            }
            self.items.swap(i, best);
            i = best;
        }
    }

    pub fn push(&mut self, value: Value, prio: f64) -> Result<Value, String> {
        let item = PqItem {
            value,
            prio,
            seq: self.seq,
        };
        self.seq += 1;
        self.items.push(item);
        let n = self.items.len();
        self.sift_up(n - 1);
        Ok(Value::Null)
    }

    pub fn pop(&mut self) -> Result<Value, String> {
        if self.items.is_empty() {
            return Err("pop on an empty priority_queue".to_string());
        }
        let n = self.items.len();
        self.items.swap(0, n - 1);
        let out = self.items.pop().unwrap();
        self.sift_down(0);
        Ok(out.value)
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
            "push" => {
                want(name, args, 2)?;
                let prio = want_num(&args[1], "priority_queue.push")?;
                self.push(args[0].clone(), prio)
            }
            "pop" => {
                want(name, args, 0)?;
                self.pop()
            }
            "peek" => {
                want(name, args, 0)?;
                match self.items.first() {
                    Some(it) => Ok(it.value.clone()),
                    None => Err("peek on an empty priority_queue".to_string()),
                }
            }
            "clone" => {
                want(name, args, 0)?;
                let mut nd = PrioQueueData::new(self.min);
                for it in &self.items {
                    nd.push(it.value.clone(), it.prio)?;
                }
                Ok(Value::PrioQueue(new_ref(nd)))
            }
            _ => Err(unknown(name, "priority_queue")),
        }
    }
}

// ------------------------------------------------------------------- graph

#[derive(Debug, Clone)]
pub struct Edge {
    pub to: usize,
    pub weight: f64,
}

/// Directed or undirected graph on an adjacency list. Node ids are map keys
/// (null, bool, int, string). `bfs` and `dfs` return visit order.
#[derive(Debug, Default, Clone)]
pub struct GraphData {
    pub directed: bool,
    pub nodes: Vec<Value>,
    pub adj: Vec<Vec<Edge>>,
}

impl GraphData {
    pub fn new(directed: bool) -> Self {
        GraphData {
            directed,
            nodes: Vec::new(),
            adj: Vec::new(),
        }
    }

    pub fn index_of(&self, v: &Value) -> Option<usize> {
        self.nodes.iter().position(|n| values_equal(n, v))
    }

    pub fn add_node(&mut self, v: Value) -> Result<Value, String> {
        if self.index_of(&v).is_some() {
            return Err(format!("node already exists: {}", short(&v)));
        }
        self.nodes.push(v);
        self.adj.push(Vec::new());
        Ok(Value::Null)
    }

    pub fn add_edge(&mut self, a: &Value, b: &Value, weight: f64) -> Result<Value, String> {
        let ia = self
            .index_of(a)
            .ok_or_else(|| format!("edge endpoint is not a node: {}", short(a)))?;
        let ib = self
            .index_of(b)
            .ok_or_else(|| format!("edge endpoint is not a node: {}", short(b)))?;
        self.adj[ia].push(Edge { to: ib, weight });
        if !self.directed {
            self.adj[ib].push(Edge { to: ia, weight });
        }
        Ok(Value::Null)
    }

    pub fn edge_count(&self) -> usize {
        let total: usize = self.adj.iter().map(|v| v.len()).sum();
        if self.directed {
            total
        } else {
            total / 2
        }
    }

    pub fn neighbors(&self, v: &Value) -> Result<Vec<Value>, String> {
        let i = self
            .index_of(v)
            .ok_or_else(|| format!("not a node: {}", short(v)))?;
        Ok(self.adj[i]
            .iter()
            .map(|e| self.nodes[e.to].clone())
            .collect())
    }

    /// BFS shortest path (unweighted hop count). `none` when unreachable.
    pub fn find_path(&self, a: &Value, b: &Value) -> Result<Value, String> {
        let start = self
            .index_of(a)
            .ok_or_else(|| format!("not a node: {}", short(a)))?;
        let goal = self
            .index_of(b)
            .ok_or_else(|| format!("not a node: {}", short(b)))?;
        let mut prev = vec![usize::MAX; self.nodes.len()];
        let mut seen = vec![false; self.nodes.len()];
        let mut queue = VecDeque::new();
        queue.push_back(start);
        seen[start] = true;
        while let Some(cur) = queue.pop_front() {
            if cur == goal {
                let mut path = vec![goal];
                let mut at = goal;
                while at != start {
                    at = prev[at];
                    path.push(at);
                }
                path.reverse();
                return Ok(Value::Option(Some(Box::new(make_list(
                    path.iter().map(|i| self.nodes[*i].clone()).collect(),
                )))));
            }
            for e in &self.adj[cur] {
                if !seen[e.to] {
                    seen[e.to] = true;
                    prev[e.to] = cur;
                    queue.push_back(e.to);
                }
            }
        }
        Ok(Value::Option(None))
    }

    pub fn bfs(&self, start: &Value) -> Result<Vec<Value>, String> {
        let s = self
            .index_of(start)
            .ok_or_else(|| format!("not a node: {}", short(start)))?;
        let mut seen = vec![false; self.nodes.len()];
        let mut out = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(s);
        seen[s] = true;
        while let Some(cur) = queue.pop_front() {
            out.push(self.nodes[cur].clone());
            for e in &self.adj[cur] {
                if !seen[e.to] {
                    seen[e.to] = true;
                    queue.push_back(e.to);
                }
            }
        }
        Ok(out)
    }

    pub fn dfs(&self, start: &Value) -> Result<Vec<Value>, String> {
        let s = self
            .index_of(start)
            .ok_or_else(|| format!("not a node: {}", short(start)))?;
        let mut seen = vec![false; self.nodes.len()];
        let mut out = Vec::new();
        let mut stack = vec![s];
        while let Some(cur) = stack.pop() {
            if seen[cur] {
                continue;
            }
            seen[cur] = true;
            out.push(self.nodes[cur].clone());
            for e in self.adj[cur].iter().rev() {
                if !seen[e.to] {
                    stack.push(e.to);
                }
            }
        }
        Ok(out)
    }

    pub fn deep_clone(&self) -> GraphData {
        GraphData {
            directed: self.directed,
            nodes: self.nodes.iter().map(deep_clone).collect(),
            adj: self.adj.clone(),
        }
    }

    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.nodes.len() as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.nodes.is_empty()))
            }
            "clear" => {
                want(name, args, 0)?;
                self.nodes.clear();
                self.adj.clear();
                Ok(Value::Null)
            }
            "is_directed" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.directed))
            }
            "add_node" => {
                want(name, args, 1)?;
                self.add_node(args[0].clone())
            }
            "has_node" => {
                want(name, args, 1)?;
                Ok(Value::Bool(self.index_of(&args[0]).is_some()))
            }
            "add_edge" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(format!(
                        "add_edge expects 2 or 3 arguments, got {}",
                        args.len()
                    ));
                }
                let w = if args.len() == 3 {
                    want_num(&args[2], "add_edge")?
                } else {
                    1.0
                };
                self.add_edge(&args[0], &args[1], w)
            }
            "neighbors" => {
                want(name, args, 1)?;
                Ok(make_list(self.neighbors(&args[0])?))
            }
            "path_exists" => {
                want(name, args, 2)?;
                let s = self
                    .index_of(&args[0])
                    .ok_or_else(|| format!("not a node: {}", short(&args[0])))?;
                let g = self
                    .index_of(&args[1])
                    .ok_or_else(|| format!("not a node: {}", short(&args[1])))?;
                let mut seen = vec![false; self.nodes.len()];
                let mut queue = VecDeque::new();
                queue.push_back(s);
                seen[s] = true;
                while let Some(cur) = queue.pop_front() {
                    if cur == g {
                        return Ok(Value::Bool(true));
                    }
                    for e in &self.adj[cur] {
                        if !seen[e.to] {
                            seen[e.to] = true;
                            queue.push_back(e.to);
                        }
                    }
                }
                Ok(Value::Bool(false))
            }
            "find_path" => {
                want(name, args, 2)?;
                self.find_path(&args[0], &args[1])
            }
            "bfs" => {
                want(name, args, 1)?;
                Ok(make_list(self.bfs(&args[0])?))
            }
            "dfs" => {
                want(name, args, 1)?;
                Ok(make_list(self.dfs(&args[0])?))
            }
            "nodes" => {
                want(name, args, 0)?;
                Ok(make_list(self.nodes.iter().map(deep_clone).collect()))
            }
            "edge_count" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.edge_count() as i64))
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::Graph(new_ref(self.deep_clone())))
            }
            _ => Err(unknown(name, "graph")),
        }
    }
}

fn short(v: &Value) -> String {
    match v {
        Value::Str(s) => format!("\"{}\"", s),
        other => super::display_value(other),
    }
}

// --------------------------------------------------------------- hash_table

pub const LOAD_FACTOR_THRESHOLD: f64 = 0.75;

/// Hash table with visible buckets. `set` rehashes past a load factor of 0.75,
/// doubling the bucket count; tests observe `bucket_count()` change.
#[derive(Debug, Default)]
pub struct HashTableData {
    pub buckets: Vec<Vec<(MapKey, Value)>>,
    pub size: usize,
}

impl HashTableData {
    pub fn new() -> Self {
        HashTableData {
            buckets: vec![Vec::new(); 8],
            size: 0,
        }
    }

    fn bucket_of(key: &MapKey, n: usize) -> usize {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        (h.finish() % n as u64) as usize
    }

    fn find(&self, key: &MapKey) -> Option<(usize, usize)> {
        let b = Self::bucket_of(key, self.buckets.len());
        self.buckets[b]
            .iter()
            .position(|(k, _)| k == key)
            .map(|i| (b, i))
    }

    fn rehash(&mut self) {
        let new_len = self.buckets.len() * 2;
        let mut buckets: Vec<Vec<(MapKey, Value)>> = vec![Vec::new(); new_len];
        for old in self.buckets.drain(..) {
            for (k, v) in old {
                let b = Self::bucket_of(&k, new_len);
                buckets[b].push((k, v));
            }
        }
        self.buckets = buckets;
    }

    pub fn set(&mut self, key: MapKey, value: Value) -> Result<Value, String> {
        if let Some((b, i)) = self.find(&key) {
            self.buckets[b][i].1 = value;
        } else {
            let b = Self::bucket_of(&key, self.buckets.len());
            self.buckets[b].push((key, value));
            self.size += 1;
            if self.size as f64 / self.buckets.len() as f64 > LOAD_FACTOR_THRESHOLD {
                self.rehash();
            }
        }
        Ok(Value::Null)
    }

    pub fn get(&self, key: &MapKey) -> Result<Value, String> {
        match self.find(key) {
            Some((b, i)) => Ok(self.buckets[b][i].1.clone()),
            None => Err(format!("missing key {}", key.display())),
        }
    }

    pub fn remove(&mut self, key: &MapKey) -> Result<Value, String> {
        match self.find(key) {
            Some((b, i)) => {
                self.size -= 1;
                Ok(self.buckets[b].remove(i).1)
            }
            None => Err(format!("missing key {}", key.display())),
        }
    }

    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.size as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.size == 0))
            }
            "clear" => {
                want(name, args, 0)?;
                for b in self.buckets.iter_mut() {
                    b.clear();
                }
                self.size = 0;
                Ok(Value::Null)
            }
            "set" => {
                want(name, args, 2)?;
                let k = super::as_map_key(&args[0])?;
                self.set(k, args[1].clone())
            }
            "get" => {
                want(name, args, 1)?;
                let k = super::as_map_key(&args[0])?;
                self.get(&k)
            }
            "remove" => {
                want(name, args, 1)?;
                let k = super::as_map_key(&args[0])?;
                self.remove(&k)
            }
            "contains" => {
                want(name, args, 1)?;
                let k = super::as_map_key(&args[0])?;
                Ok(Value::Bool(self.find(&k).is_some()))
            }
            "bucket_count" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.buckets.len() as i64))
            }
            "load_factor" => {
                want(name, args, 0)?;
                Ok(Value::Float(self.size as f64 / self.buckets.len() as f64))
            }
            "keys" => {
                want(name, args, 0)?;
                let mut out = Vec::new();
                for b in &self.buckets {
                    for (k, _) in b {
                        out.push(key_to_value(k));
                    }
                }
                Ok(make_list(out))
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::HashTable(new_ref(HashTableData {
                    buckets: self
                        .buckets
                        .iter()
                        .map(|b| b.iter().map(|(k, v)| (k.clone(), deep_clone(v))).collect())
                        .collect(),
                    size: self.size,
                })))
            }
            _ => Err(unknown(name, "hash_table")),
        }
    }
}

// -------------------------------------------------------------------- trie

pub type TrieRef = Arc<Mutex<TrieNode>>;

#[derive(Debug, Default)]
pub struct TrieNode {
    /// BTreeMap keeps `with_prefix` output in sorted order.
    pub children: BTreeMap<char, TrieRef>,
    pub end: bool,
}

#[derive(Debug, Default)]
pub struct TrieData {
    pub root: TrieRef,
    pub count: usize,
}

impl TrieData {
    pub fn new() -> Self {
        TrieData {
            root: Arc::new(Mutex::new(TrieNode::default())),
            count: 0,
        }
    }

    pub fn insert(&mut self, word: &str) -> Result<Value, String> {
        let mut node = self.root.clone();
        for ch in word.chars() {
            let next = {
                let mut nb = node.lock().unwrap();
                nb.children
                    .entry(ch)
                    .or_insert_with(|| Arc::new(Mutex::new(TrieNode::default())))
                    .clone()
            };
            node = next;
        }
        let was_new = !node.lock().unwrap().end;
        node.lock().unwrap().end = true;
        if was_new {
            self.count += 1;
        }
        Ok(Value::Null)
    }

    fn walk_to(&self, prefix: &str) -> Option<TrieRef> {
        let mut node = self.root.clone();
        for ch in prefix.chars() {
            let next = node.lock().unwrap().children.get(&ch).cloned()?;
            node = next;
        }
        Some(node)
    }

    /// Every word in the trie, sorted (children are a BTreeMap).
    pub fn all_words(&self) -> Vec<String> {
        let mut out = Vec::new();
        Self::collect(&self.root, "", &mut out);
        out
    }

    fn collect(node: &TrieRef, prefix: &str, out: &mut Vec<String>) {
        let (children, end) = {
            let b = node.lock().unwrap();
            (b.children.clone(), b.end)
        };
        if end {
            out.push(prefix.to_string());
        }
        for (ch, child) in children {
            let mut p = prefix.to_string();
            p.push(ch);
            Self::collect(&child, &p, out);
        }
    }

    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.count as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.count == 0))
            }
            "clear" => {
                want(name, args, 0)?;
                self.root = Arc::new(Mutex::new(TrieNode::default()));
                self.count = 0;
                Ok(Value::Null)
            }
            "all_words" => {
                want(name, args, 0)?;
                Ok(join_words(&self.all_words()))
            }
            "insert" => {
                want(name, args, 1)?;
                let w = want_str(&args[0], "trie.insert")?;
                self.insert(&w)
            }
            "contains" => {
                want(name, args, 1)?;
                let w = want_str(&args[0], "trie.contains")?;
                match self.walk_to(&w) {
                    Some(n) => Ok(Value::Bool(n.lock().unwrap().end)),
                    None => Ok(Value::Bool(false)),
                }
            }
            "starts_with" => {
                want(name, args, 1)?;
                let w = want_str(&args[0], "trie.starts_with")?;
                Ok(Value::Bool(self.walk_to(&w).is_some()))
            }
            "with_prefix" => {
                want(name, args, 1)?;
                let w = want_str(&args[0], "trie.with_prefix")?;
                match self.walk_to(&w) {
                    Some(n) => {
                        let mut out = Vec::new();
                        Self::collect(&n, &w, &mut out);
                        Ok(join_words(&out))
                    }
                    None => Ok(join_words(&[])),
                }
            }
            "clone" => {
                want(name, args, 0)?;
                let mut nd = TrieData::new();
                let mut words = Vec::new();
                Self::collect(&self.root, "", &mut words);
                for w in words {
                    nd.insert(&w)?;
                }
                Ok(Value::Trie(new_ref(nd)))
            }
            _ => Err(unknown(name, "trie")),
        }
    }
}

// ------------------------------------------------------------------ vector

/// Allocation cap for a single vector, mirroring the 16M-element caps on
/// `MatrixData` (16,777,216 f64 elements = 128 MiB).
pub const MAX_VECTOR_ELEMENTS: usize = 16_777_216;

/// Contiguous 1D numeric vector.
#[derive(Debug, Clone)]
pub struct VectorData {
    pub data: Vec<f64>,
}

pub fn resolve_vector_index(idx: &Value, len: usize, who: &str) -> Result<usize, String> {
    match idx {
        Value::Int(i) => {
            if *i >= 0 {
                let u = *i as usize;
                if u < len {
                    Ok(u)
                } else {
                    Err(format!("index {} out of bounds for length {}", i, len))
                }
            } else {
                let neg = i.unsigned_abs() as usize;
                if neg <= len && neg > 0 {
                    Ok(len - neg)
                } else {
                    Err(format!("index {} out of bounds for length {}", i, len))
                }
            }
        }
        other => Err(format!(
            "{} expects an int index, found {}",
            who,
            super::type_name(other)
        )),
    }
}

fn resolve_slice_bound(v: &Value, len: usize, who: &str) -> Result<usize, String> {
    match v {
        Value::Int(i) => {
            if *i >= 0 {
                Ok((*i as usize).min(len))
            } else {
                let neg = i.unsigned_abs() as usize;
                if neg <= len {
                    Ok(len - neg)
                } else {
                    Ok(0)
                }
            }
        }
        other => Err(format!(
            "{} expects an int index, found {}",
            who,
            super::type_name(other)
        )),
    }
}

impl VectorData {
    pub fn new(len: usize, fill: f64) -> Self {
        let actual_len = len.min(MAX_VECTOR_ELEMENTS);
        VectorData {
            data: vec![fill; actual_len],
        }
    }

    pub fn elementwise_op<F>(&self, name: &str, arg: &Value, f: F) -> Result<Value, String>
    where
        F: Fn(f64, f64) -> Result<f64, String>,
    {
        if let Value::Vector(o) = arg {
            let other = o.lock().unwrap();
            if self.data.len() != other.data.len() {
                return Err(format!(
                    "vector {} length mismatch: {} and {}",
                    name,
                    self.data.len(),
                    other.data.len()
                ));
            }
            let mut data = Vec::with_capacity(self.data.len());
            for (&a, &b) in self.data.iter().zip(other.data.iter()) {
                data.push(f(a, b)?);
            }
            Ok(Value::Vector(new_ref(VectorData { data })))
        } else if let Ok(scalar) = want_num(arg, &format!("vector.{}", name)) {
            let mut data = Vec::with_capacity(self.data.len());
            for &a in &self.data {
                data.push(f(a, scalar)?);
            }
            Ok(Value::Vector(new_ref(VectorData { data })))
        } else {
            Err(format!(
                "vector {} expects a vector or number, found {}",
                name,
                super::type_name(arg)
            ))
        }
    }

    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.data.len() as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.data.is_empty()))
            }
            "get" => {
                want(name, args, 1)?;
                let idx = resolve_vector_index(&args[0], self.data.len(), "vector.get")?;
                Ok(Value::Float(self.data[idx]))
            }
            "set" => {
                want(name, args, 2)?;
                let idx = resolve_vector_index(&args[0], self.data.len(), "vector.set")?;
                let val = want_num(&args[1], "vector.set")?;
                self.data[idx] = val;
                Ok(Value::Null)
            }
            "push" => {
                want(name, args, 1)?;
                let val = want_num(&args[0], "vector.push")?;
                if self.data.len() >= MAX_VECTOR_ELEMENTS {
                    return Err(format!(
                        "vector cannot exceed {} elements",
                        MAX_VECTOR_ELEMENTS
                    ));
                }
                self.data.push(val);
                Ok(Value::Null)
            }
            "slice" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(format!(
                        "vector.slice expects 1 or 2 arguments, got {}",
                        args.len()
                    ));
                }
                let start = resolve_slice_bound(&args[0], self.data.len(), "vector.slice")?;
                let end = if args.len() == 2 {
                    resolve_slice_bound(&args[1], self.data.len(), "vector.slice")?
                } else {
                    self.data.len()
                };
                if start > end {
                    return Err(format!("slice range {}..{} out of bounds", start, end));
                }
                Ok(Value::Vector(new_ref(VectorData {
                    data: self.data[start..end].to_vec(),
                })))
            }
            "to_list" => {
                want(name, args, 0)?;
                let items: Vec<Value> = self.data.iter().map(|&x| Value::Float(x)).collect();
                Ok(make_list(items))
            }
            "copy" | "clone" => {
                want(name, args, 0)?;
                Ok(Value::Vector(new_ref(self.clone())))
            }
            "add" => {
                want(name, args, 1)?;
                self.elementwise_op("add", &args[0], |a, b| Ok(a + b))
            }
            "sub" => {
                want(name, args, 1)?;
                self.elementwise_op("sub", &args[0], |a, b| Ok(a - b))
            }
            "mul" => {
                want(name, args, 1)?;
                self.elementwise_op("mul", &args[0], |a, b| Ok(a * b))
            }
            "div" => {
                want(name, args, 1)?;
                self.elementwise_op("div", &args[0], |a, b| {
                    if b == 0.0 {
                        Err("division by zero".to_string())
                    } else {
                        Ok(a / b)
                    }
                })
            }
            "sum" => {
                want(name, args, 0)?;
                let mut sum: f64 = 0.0;
                for &x in &self.data {
                    sum += x;
                }
                if sum == 0.0 {
                    sum = 0.0; // eliminate -0.0
                }
                Ok(Value::Float(sum))
            }
            "mean" => {
                want(name, args, 0)?;
                if self.data.is_empty() {
                    return Err("cannot compute mean of empty vector".to_string());
                }
                let sum: f64 = self.data.iter().sum();
                Ok(Value::Float(sum / self.data.len() as f64))
            }
            "variance" => {
                want(name, args, 0)?;
                if self.data.is_empty() {
                    return Err("cannot compute variance of empty vector".to_string());
                }
                let n = self.data.len() as f64;
                let mean: f64 = self.data.iter().sum::<f64>() / n;
                let var: f64 = self.data.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
                Ok(Value::Float(var))
            }
            "std" => {
                want(name, args, 0)?;
                if self.data.is_empty() {
                    return Err("cannot compute std of empty vector".to_string());
                }
                let n = self.data.len() as f64;
                let mean: f64 = self.data.iter().sum::<f64>() / n;
                let var: f64 = self.data.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
                Ok(Value::Float(var.sqrt()))
            }
            "min" => {
                want(name, args, 0)?;
                if self.data.is_empty() {
                    return Err("cannot compute min of empty vector".to_string());
                }
                let mut min_val = self.data[0];
                for &x in &self.data[1..] {
                    if x.total_cmp(&min_val) == std::cmp::Ordering::Less {
                        min_val = x;
                    }
                }
                Ok(Value::Float(min_val))
            }
            "max" => {
                want(name, args, 0)?;
                if self.data.is_empty() {
                    return Err("cannot compute max of empty vector".to_string());
                }
                let mut max_val = self.data[0];
                for &x in &self.data[1..] {
                    if x.total_cmp(&max_val) == std::cmp::Ordering::Greater {
                        max_val = x;
                    }
                }
                Ok(Value::Float(max_val))
            }
            "argmin" => {
                want(name, args, 0)?;
                if self.data.is_empty() {
                    return Err("cannot compute argmin of empty vector".to_string());
                }
                let mut best_i = 0;
                let mut best_val = self.data[0];
                for (i, &x) in self.data.iter().enumerate().skip(1) {
                    if x.total_cmp(&best_val) == std::cmp::Ordering::Less {
                        best_val = x;
                        best_i = i;
                    }
                }
                Ok(Value::Int(best_i as i64))
            }
            "argmax" => {
                want(name, args, 0)?;
                if self.data.is_empty() {
                    return Err("cannot compute argmax of empty vector".to_string());
                }
                let mut best_i = 0;
                let mut best_val = self.data[0];
                for (i, &x) in self.data.iter().enumerate().skip(1) {
                    if x.total_cmp(&best_val) == std::cmp::Ordering::Greater {
                        best_val = x;
                        best_i = i;
                    }
                }
                Ok(Value::Int(best_i as i64))
            }
            "dot" => {
                want(name, args, 1)?;
                let other_data = match &args[0] {
                    Value::Vector(v) => v.lock().unwrap().data.clone(),
                    other => {
                        return Err(format!(
                            "vector dot expects a vector, found {}",
                            super::type_name(other)
                        ))
                    }
                };
                if self.data.len() != other_data.len() {
                    return Err(format!(
                        "vector dot length mismatch: {} and {}",
                        self.data.len(),
                        other_data.len()
                    ));
                }
                let sum: f64 = self
                    .data
                    .iter()
                    .zip(other_data.iter())
                    .map(|(a, b)| a * b)
                    .sum();
                Ok(Value::Float(sum))
            }
            "norm" => {
                want(name, args, 0)?;
                let sum_sq: f64 = self.data.iter().map(|x| x * x).sum();
                Ok(Value::Float(sum_sq.sqrt()))
            }
            "cumsum" => {
                want(name, args, 0)?;
                let mut data = Vec::with_capacity(self.data.len());
                let mut acc = 0.0;
                for &x in &self.data {
                    acc += x;
                    data.push(acc);
                }
                Ok(Value::Vector(new_ref(VectorData { data })))
            }
            "diff" => {
                want(name, args, 0)?;
                if self.data.len() < 2 {
                    return Ok(Value::Vector(new_ref(VectorData { data: Vec::new() })));
                }
                let mut data = Vec::with_capacity(self.data.len() - 1);
                for i in 0..self.data.len() - 1 {
                    data.push(self.data[i + 1] - self.data[i]);
                }
                Ok(Value::Vector(new_ref(VectorData { data })))
            }
            "sort" => {
                want(name, args, 0)?;
                self.data.sort_by(|a, b| a.total_cmp(b));
                Ok(Value::Null)
            }
            "argsort" => {
                want(name, args, 0)?;
                let mut indices: Vec<usize> = (0..self.data.len()).collect();
                indices.sort_by(|&i, &j| self.data[i].total_cmp(&self.data[j]));
                Ok(make_list(
                    indices.into_iter().map(|i| Value::Int(i as i64)).collect(),
                ))
            }
            "save_npy" => {
                want(name, args, 1)?;
                let path = want_str(&args[0], "vector.save_npy")?;
                save_npy_file(&path, &[self.data.len()], &self.data)?;
                Ok(Value::Result(Ok(Box::new(Value::Null))))
            }
            _ => Err(unknown(name, "vector")),
        }
    }
}

// ------------------------------------------------------------------ matrix

/// Row-major numeric grid. `add` and `mul` check dimensions.
#[derive(Debug, Clone)]
pub struct MatrixData {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f64>,
}

/// Allocation cap for a single matrix, mirroring the 16M-element caps on
/// `string.repeat` / `pad_*` (16,777,216 f64 cells = 128 MiB).
pub const MAX_MATRIX_CELLS: usize = 16_777_216;

/// Validates dimensions and returns the cell count, rejecting overflow and
/// matrices above `MAX_MATRIX_CELLS` with a clean, catchable error.
pub fn check_matrix_dims(rows: usize, cols: usize) -> Result<usize, String> {
    match rows.checked_mul(cols) {
        Some(cells) if cells <= MAX_MATRIX_CELLS => Ok(cells),
        Some(cells) => Err(format!(
            "matrix of {}x{} would need {} cells (max {})",
            rows, cols, cells, MAX_MATRIX_CELLS
        )),
        None => Err(format!(
            "matrix of {}x{} is too large (max {} cells)",
            rows, cols, MAX_MATRIX_CELLS
        )),
    }
}

pub fn matrix_from_list_val(val: &Value) -> Result<Value, String> {
    let rows: Vec<Value> = match val {
        Value::List(l) | Value::VecDyn(l) => l.lock().unwrap().items.clone(),
        Value::Tuple(t) => t.lock().unwrap().clone(),
        other => {
            return Err(format!(
                "matrix_from_list expects a list of rows, found {}",
                super::type_name(other)
            ))
        }
    };
    if rows.is_empty() {
        return Ok(Value::Matrix(new_ref(MatrixData::new(0, 0, 0.0))));
    }
    let mut cols_n: Option<usize> = None;
    let mut data = Vec::new();

    for (r_idx, row_val) in rows.iter().enumerate() {
        let (row_len, items) = match row_val {
            Value::List(l) | Value::VecDyn(l) => {
                let items = l.lock().unwrap().items.clone();
                let len = items.len();
                let mut nums = Vec::with_capacity(len);
                for it in &items {
                    nums.push(want_num(it, "matrix_from_list")?);
                }
                (len, nums)
            }
            Value::Vector(v) => {
                let d = v.lock().unwrap().data.clone();
                (d.len(), d)
            }
            Value::Tuple(t) => {
                let items = t.lock().unwrap().clone();
                let len = items.len();
                let mut nums = Vec::with_capacity(len);
                for it in &items {
                    nums.push(want_num(it, "matrix_from_list")?);
                }
                (len, nums)
            }
            other => {
                return Err(format!(
                    "matrix_from_list row {} must be a list or vector, found {}",
                    r_idx,
                    super::type_name(other)
                ))
            }
        };

        if let Some(c) = cols_n {
            if row_len != c {
                return Err(format!(
                    "matrix_from_list row {} length mismatch (expected {}, got {})",
                    r_idx, c, row_len
                ));
            }
        } else {
            cols_n = Some(row_len);
        }
        data.extend(items);
    }

    let cols = cols_n.unwrap_or(0);
    check_matrix_dims(rows.len(), cols)?;
    Ok(Value::Matrix(new_ref(MatrixData {
        rows: rows.len(),
        cols,
        data,
    })))
}

fn solve_vec_lu(lu: &[f64], p: &[usize], b: &[f64]) -> Vec<f64> {
    let n = p.len();
    let mut y = vec![0.0; n];
    for i in 0..n {
        let mut sum = b[p[i]];
        for j in 0..i {
            sum -= lu[i * n + j] * y[j];
        }
        y[i] = sum;
    }
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let mut sum = y[i];
        for j in (i + 1)..n {
            sum -= lu[i * n + j] * x[j];
        }
        x[i] = sum / lu[i * n + i];
    }
    x
}

impl MatrixData {
    pub fn new(rows: usize, cols: usize, fill: f64) -> Self {
        let cells = check_matrix_dims(rows, cols).unwrap_or(0);
        MatrixData {
            rows,
            cols,
            data: vec![fill; cells],
        }
    }

    fn check(&self, r: usize, c: usize) -> Result<(), String> {
        if r >= self.rows || c >= self.cols {
            return Err(format!(
                "index ({}, {}) out of bounds for {}x{} matrix",
                r, c, self.rows, self.cols
            ));
        }
        Ok(())
    }

    pub fn get(&self, r: usize, c: usize) -> Result<Value, String> {
        self.check(r, c)?;
        Ok(Value::Float(self.data[r * self.cols + c]))
    }

    pub fn set(&mut self, r: usize, c: usize, v: f64) -> Result<Value, String> {
        self.check(r, c)?;
        self.data[r * self.cols + c] = v;
        Ok(Value::Null)
    }

    pub fn row(&self, r: usize) -> Result<Value, String> {
        if r >= self.rows {
            return Err(format!("row {} out of bounds for {} rows", r, self.rows));
        }
        let items: Vec<Value> = (0..self.cols)
            .map(|c| Value::Float(self.data[r * self.cols + c]))
            .collect();
        Ok(make_list(items))
    }

    pub fn col(&self, c: usize) -> Result<Value, String> {
        if c >= self.cols {
            return Err(format!("col {} out of bounds for {} cols", c, self.cols));
        }
        let items: Vec<Value> = (0..self.rows)
            .map(|r| Value::Float(self.data[r * self.cols + c]))
            .collect();
        Ok(make_list(items))
    }

    pub fn to_list(&self) -> Result<Value, String> {
        let mut rows = Vec::with_capacity(self.rows);
        for r in 0..self.rows {
            let mut row = Vec::with_capacity(self.cols);
            for c in 0..self.cols {
                row.push(Value::Float(self.data[r * self.cols + c]));
            }
            rows.push(make_list(row));
        }
        Ok(make_list(rows))
    }

    pub fn transpose(&self) -> Result<Value, String> {
        let cells = check_matrix_dims(self.cols, self.rows)?;
        let mut data = vec![0.0; cells];
        for r in 0..self.rows {
            for c in 0..self.cols {
                data[c * self.rows + r] = self.data[r * self.cols + c];
            }
        }
        Ok(Value::Matrix(new_ref(MatrixData {
            rows: self.cols,
            cols: self.rows,
            data,
        })))
    }

    pub fn scale(&self, scalar: f64) -> Result<Value, String> {
        let data: Vec<f64> = self.data.iter().map(|&x| x * scalar).collect();
        Ok(Value::Matrix(new_ref(MatrixData {
            rows: self.rows,
            cols: self.cols,
            data,
        })))
    }

    pub fn add(&self, other: &MatrixData) -> Result<Value, String> {
        if self.rows != other.rows || self.cols != other.cols {
            return Err(format!(
                "matrix add needs equal dimensions, got {}x{} and {}x{}",
                self.rows, self.cols, other.rows, other.cols
            ));
        }
        let data: Vec<f64> = self
            .data
            .iter()
            .zip(other.data.iter())
            .map(|(a, b)| a + b)
            .collect();
        Ok(Value::Matrix(new_ref(MatrixData {
            rows: self.rows,
            cols: self.cols,
            data,
        })))
    }

    pub fn sub(&self, other: &MatrixData) -> Result<Value, String> {
        if self.rows != other.rows || self.cols != other.cols {
            return Err(format!(
                "matrix sub needs equal dimensions, got {}x{} and {}x{}",
                self.rows, self.cols, other.rows, other.cols
            ));
        }
        let data: Vec<f64> = self
            .data
            .iter()
            .zip(other.data.iter())
            .map(|(a, b)| a - b)
            .collect();
        Ok(Value::Matrix(new_ref(MatrixData {
            rows: self.rows,
            cols: self.cols,
            data,
        })))
    }

    pub fn hadamard(&self, other: &MatrixData) -> Result<Value, String> {
        if self.rows != other.rows || self.cols != other.cols {
            return Err(format!(
                "matrix hadamard needs equal dimensions, got {}x{} and {}x{}",
                self.rows, self.cols, other.rows, other.cols
            ));
        }
        let data: Vec<f64> = self
            .data
            .iter()
            .zip(other.data.iter())
            .map(|(a, b)| a * b)
            .collect();
        Ok(Value::Matrix(new_ref(MatrixData {
            rows: self.rows,
            cols: self.cols,
            data,
        })))
    }

    pub fn mul(&self, other: &MatrixData) -> Result<Value, String> {
        if self.cols != other.rows {
            return Err(format!(
                "matrix mul needs {} cols to match {} rows",
                self.cols, other.rows
            ));
        }
        let cells = check_matrix_dims(self.rows, other.cols)?;
        let mut data = vec![0.0; cells];
        for i in 0..self.rows {
            for j in 0..other.cols {
                let mut sum = 0.0;
                for k in 0..self.cols {
                    sum += self.data[i * self.cols + k] * other.data[k * other.cols + j];
                }
                data[i * other.cols + j] = sum;
            }
        }
        Ok(Value::Matrix(new_ref(MatrixData {
            rows: self.rows,
            cols: other.cols,
            data,
        })))
    }

    pub fn trace(&self) -> Result<Value, String> {
        if self.rows != self.cols {
            return Err(format!(
                "matrix trace requires square matrix, got {}x{}",
                self.rows, self.cols
            ));
        }
        let tr: f64 = (0..self.rows).map(|i| self.data[i * self.cols + i]).sum();
        Ok(Value::Float(tr))
    }

    pub fn sum(&self, axis: Option<i64>) -> Result<Value, String> {
        match axis {
            None => {
                let total: f64 = self.data.iter().sum();
                Ok(Value::Float(total))
            }
            Some(0) => {
                let mut res = vec![0.0; self.cols];
                for r in 0..self.rows {
                    for (c, slot) in res.iter_mut().enumerate() {
                        *slot += self.data[r * self.cols + c];
                    }
                }
                Ok(Value::Vector(new_ref(VectorData { data: res })))
            }
            Some(1) => {
                let mut res = vec![0.0; self.rows];
                for (r, slot) in res.iter_mut().enumerate() {
                    for c in 0..self.cols {
                        *slot += self.data[r * self.cols + c];
                    }
                }
                Ok(Value::Vector(new_ref(VectorData { data: res })))
            }
            Some(a) => Err(format!("matrix sum axis must be 0, 1, or null, got {}", a)),
        }
    }

    pub fn mean(&self, axis: Option<i64>) -> Result<Value, String> {
        match axis {
            None => {
                if self.rows == 0 || self.cols == 0 {
                    return Err("cannot compute mean of empty matrix".to_string());
                }
                let total: f64 = self.data.iter().sum();
                Ok(Value::Float(total / (self.rows * self.cols) as f64))
            }
            Some(0) => {
                if self.rows == 0 {
                    return Err("cannot compute mean with 0 rows".to_string());
                }
                let mut res = vec![0.0; self.cols];
                for r in 0..self.rows {
                    for (c, slot) in res.iter_mut().enumerate() {
                        *slot += self.data[r * self.cols + c];
                    }
                }
                for x in &mut res {
                    *x /= self.rows as f64;
                }
                Ok(Value::Vector(new_ref(VectorData { data: res })))
            }
            Some(1) => {
                if self.cols == 0 {
                    return Err("cannot compute mean with 0 cols".to_string());
                }
                let mut res = vec![0.0; self.rows];
                for (r, slot) in res.iter_mut().enumerate() {
                    for c in 0..self.cols {
                        *slot += self.data[r * self.cols + c];
                    }
                }
                for x in &mut res {
                    *x /= self.cols as f64;
                }
                Ok(Value::Vector(new_ref(VectorData { data: res })))
            }
            Some(a) => Err(format!("matrix mean axis must be 0, 1, or null, got {}", a)),
        }
    }

    pub fn lu_decompose(&self) -> Result<(Vec<f64>, Vec<usize>, usize), String> {
        if self.rows != self.cols {
            return Err(format!(
                "requires square matrix, got {}x{}",
                self.rows, self.cols
            ));
        }
        let n = self.rows;
        if n == 0 {
            return Err("cannot decompose empty matrix".to_string());
        }
        let mut lu = self.data.clone();
        let mut p: Vec<usize> = (0..n).collect();
        let mut num_swaps = 0;

        for k in 0..n {
            let mut max_val = 0.0;
            let mut pivot_row = k;
            for i in k..n {
                let val = lu[i * n + k].abs();
                if val > max_val {
                    max_val = val;
                    pivot_row = i;
                }
            }

            if max_val < 1e-14 {
                return Err("matrix is singular".to_string());
            }

            if pivot_row != k {
                p.swap(k, pivot_row);
                for j in 0..n {
                    lu.swap(k * n + j, pivot_row * n + j);
                }
                num_swaps += 1;
            }

            let pivot = lu[k * n + k];
            for i in (k + 1)..n {
                let factor = lu[i * n + k] / pivot;
                lu[i * n + k] = factor;
                for j in (k + 1)..n {
                    lu[i * n + j] -= factor * lu[k * n + j];
                }
            }
        }
        Ok((lu, p, num_swaps))
    }

    pub fn solve(&self, b_val: &Value) -> Result<Value, String> {
        let (lu, p, _) = self.lu_decompose()?;
        let n = self.rows;

        match b_val {
            Value::Vector(v) => {
                let v = v.lock().unwrap();
                if v.data.len() != n {
                    return Err(format!(
                        "matrix solve dimension mismatch: matrix has {} rows, vector has {}",
                        n,
                        v.data.len()
                    ));
                }
                let x = solve_vec_lu(&lu, &p, &v.data);
                Ok(Value::Vector(new_ref(VectorData { data: x })))
            }
            Value::Matrix(bm) => {
                let bm = bm.lock().unwrap();
                if bm.rows != n {
                    return Err(format!(
                        "matrix solve dimension mismatch: matrix A has {} rows, matrix B has {}",
                        n, bm.rows
                    ));
                }
                let cols = bm.cols;
                let cells = check_matrix_dims(n, cols)?;
                let mut res = vec![0.0; cells];
                for c in 0..cols {
                    let col_b: Vec<f64> = (0..n).map(|r| bm.data[r * cols + c]).collect();
                    let x = solve_vec_lu(&lu, &p, &col_b);
                    for r in 0..n {
                        res[r * cols + c] = x[r];
                    }
                }
                Ok(Value::Matrix(new_ref(MatrixData {
                    rows: n,
                    cols,
                    data: res,
                })))
            }
            Value::List(l) | Value::VecDyn(l) => {
                let items = l.lock().unwrap().items.clone();
                if items.len() != n {
                    return Err(format!(
                        "matrix solve dimension mismatch: matrix has {} rows, list has {}",
                        n,
                        items.len()
                    ));
                }
                let mut b = Vec::with_capacity(n);
                for it in &items {
                    b.push(want_num(it, "matrix.solve")?);
                }
                let x = solve_vec_lu(&lu, &p, &b);
                Ok(Value::Vector(new_ref(VectorData { data: x })))
            }
            other => Err(format!(
                "matrix solve expects a vector or matrix, found {}",
                super::type_name(other)
            )),
        }
    }

    pub fn inverse(&self) -> Result<Value, String> {
        let (lu, p, _) = self.lu_decompose()?;
        let n = self.rows;
        let cells = check_matrix_dims(n, n)?;
        let mut inv_data = vec![0.0; cells];
        for col in 0..n {
            let mut e = vec![0.0; n];
            e[col] = 1.0;
            let x = solve_vec_lu(&lu, &p, &e);
            for row in 0..n {
                inv_data[row * n + col] = x[row];
            }
        }
        Ok(Value::Matrix(new_ref(MatrixData {
            rows: n,
            cols: n,
            data: inv_data,
        })))
    }

    pub fn det(&self) -> Result<Value, String> {
        if self.rows != self.cols {
            return Err(format!(
                "matrix det requires square matrix, got {}x{}",
                self.rows, self.cols
            ));
        }
        let n = self.rows;
        if n == 0 {
            return Ok(Value::Float(1.0));
        }
        if n == 1 {
            return Ok(Value::Float(self.data[0]));
        }

        let mut lu = self.data.clone();
        let mut sign = 1.0;

        for k in 0..n {
            let mut max_val = 0.0;
            let mut pivot_row = k;
            for i in k..n {
                let val = lu[i * n + k].abs();
                if val > max_val {
                    max_val = val;
                    pivot_row = i;
                }
            }

            if max_val < 1e-15 {
                return Ok(Value::Float(0.0));
            }

            if pivot_row != k {
                for j in 0..n {
                    lu.swap(k * n + j, pivot_row * n + j);
                }
                sign = -sign;
            }

            let pivot = lu[k * n + k];
            for i in (k + 1)..n {
                let factor = lu[i * n + k] / pivot;
                lu[i * n + k] = factor;
                for j in (k + 1)..n {
                    lu[i * n + j] -= factor * lu[k * n + j];
                }
            }
        }

        let mut det = sign;
        for i in 0..n {
            det *= lu[i * n + i];
        }
        Ok(Value::Float(det))
    }

    pub fn eigen_sym(&self) -> Result<Value, String> {
        if self.rows != self.cols {
            return Err(format!(
                "eigen_sym requires square matrix, got {}x{}",
                self.rows, self.cols
            ));
        }
        let n = self.rows;
        if n == 0 {
            let entries = vec![
                (
                    MapKey::Str("values".into()),
                    Value::Vector(new_ref(VectorData { data: Vec::new() })),
                ),
                (
                    MapKey::Str("vectors".into()),
                    Value::Matrix(new_ref(MatrixData::new(0, 0, 0.0))),
                ),
            ];
            return Ok(Value::Map(new_ref(MapData::from_entries(entries))));
        }

        for i in 0..n {
            for j in (i + 1)..n {
                let diff = (self.data[i * n + j] - self.data[j * n + i]).abs();
                let scale = 1.0 + self.data[i * n + j].abs() + self.data[j * n + i].abs();
                if diff > 1e-9 * scale {
                    return Err("eigen_sym requires a symmetric matrix".to_string());
                }
            }
        }

        let mut a = self.data.clone();
        let mut v = vec![0.0; n * n];
        for i in 0..n {
            v[i * n + i] = 1.0;
        }

        let max_sweeps = 100;
        for _ in 0..max_sweeps {
            let mut off_diag_sum = 0.0;
            for i in 0..n {
                for j in (i + 1)..n {
                    off_diag_sum += a[i * n + j].abs();
                }
            }
            if off_diag_sum < 1e-13 {
                break;
            }

            for p in 0..n {
                for q in (p + 1)..n {
                    let apq = a[p * n + q];
                    if apq.abs() < 1e-15 {
                        continue;
                    }
                    let app = a[p * n + p];
                    let aqq = a[q * n + q];
                    let tau = (aqq - app) / (2.0 * apq);
                    let t = if tau >= 0.0 {
                        1.0 / (tau + (1.0 + tau * tau).sqrt())
                    } else {
                        -1.0 / (-tau + (1.0 + tau * tau).sqrt())
                    };
                    let c = 1.0 / (1.0 + t * t).sqrt();
                    let s = t * c;

                    a[p * n + p] = app - t * apq;
                    a[q * n + q] = aqq + t * apq;
                    a[p * n + q] = 0.0;
                    a[q * n + p] = 0.0;

                    for r in 0..n {
                        if r != p && r != q {
                            let arp = a[r * n + p];
                            let arq = a[r * n + q];
                            let new_arp = c * arp - s * arq;
                            let new_arq = s * arp + c * arq;
                            a[r * n + p] = new_arp;
                            a[p * n + r] = new_arp;
                            a[r * n + q] = new_arq;
                            a[q * n + r] = new_arq;
                        }
                    }

                    for r in 0..n {
                        let vrp = v[r * n + p];
                        let vrq = v[r * n + q];
                        v[r * n + p] = c * vrp - s * vrq;
                        v[r * n + q] = s * vrp + c * vrq;
                    }
                }
            }
        }

        let eigenvalues: Vec<f64> = (0..n).map(|i| a[i * n + i]).collect();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&i, &j| eigenvalues[i].total_cmp(&eigenvalues[j]));

        let sorted_values: Vec<f64> = order.iter().map(|&i| eigenvalues[i]).collect();
        let mut sorted_v = vec![0.0; n * n];
        for (new_col, &old_col) in order.iter().enumerate() {
            for r in 0..n {
                sorted_v[r * n + new_col] = v[r * n + old_col];
            }
        }

        let entries = vec![
            (
                MapKey::Str("values".into()),
                Value::Vector(new_ref(VectorData {
                    data: sorted_values,
                })),
            ),
            (
                MapKey::Str("vectors".into()),
                Value::Matrix(new_ref(MatrixData {
                    rows: n,
                    cols: n,
                    data: sorted_v,
                })),
            ),
        ];
        Ok(Value::Map(new_ref(MapData::from_entries(entries))))
    }

    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "rows" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.rows as i64))
            }
            "cols" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.cols as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.rows == 0 || self.cols == 0))
            }
            "set" => {
                want(name, args, 3)?;
                let r = want_index(&args[0], "matrix.set")?;
                let c = want_index(&args[1], "matrix.set")?;
                let v = want_num(&args[2], "matrix.set")?;
                self.set(r, c, v)
            }
            "get" => {
                want(name, args, 2)?;
                let r = want_index(&args[0], "matrix.get")?;
                let c = want_index(&args[1], "matrix.get")?;
                self.get(r, c)
            }
            "row" => {
                want(name, args, 1)?;
                let r = want_index(&args[0], "matrix.row")?;
                self.row(r)
            }
            "col" => {
                want(name, args, 1)?;
                let c = want_index(&args[0], "matrix.col")?;
                self.col(c)
            }
            "to_list" => {
                want(name, args, 0)?;
                self.to_list()
            }
            "transpose" => {
                want(name, args, 0)?;
                self.transpose()
            }
            "scale" => {
                want(name, args, 1)?;
                let s = want_num(&args[0], "matrix.scale")?;
                self.scale(s)
            }
            "add" | "mul" | "sub" | "hadamard" => {
                want(name, args, 1)?;
                let other = match &args[0] {
                    Value::Matrix(m) => m.lock().unwrap().clone(),
                    o => return Err(format!("expected a matrix, found {}", super::type_name(o))),
                };
                match name {
                    "add" => self.add(&other),
                    "mul" => self.mul(&other),
                    "sub" => self.sub(&other),
                    "hadamard" => self.hadamard(&other),
                    _ => unreachable!(),
                }
            }
            "trace" => {
                want(name, args, 0)?;
                self.trace()
            }
            "sum" => {
                if args.is_empty() || matches!(args[0], Value::Null) {
                    self.sum(None)
                } else if args.len() == 1 {
                    let a = want_int(&args[0], "matrix.sum")?;
                    self.sum(Some(a))
                } else {
                    Err(format!("matrix.sum expects 0 or 1 argument, got {}", args.len()))
                }
            }
            "mean" => {
                if args.is_empty() || matches!(args[0], Value::Null) {
                    self.mean(None)
                } else if args.len() == 1 {
                    let a = want_int(&args[0], "matrix.mean")?;
                    self.mean(Some(a))
                } else {
                    Err(format!("matrix.mean expects 0 or 1 argument, got {}", args.len()))
                }
            }
            "solve" => {
                want(name, args, 1)?;
                self.solve(&args[0])
            }
            "inverse" => {
                want(name, args, 0)?;
                self.inverse()
            }
            "det" => {
                want(name, args, 0)?;
                self.det()
            }
            "eigen_sym" => {
                want(name, args, 0)?;
                self.eigen_sym()
            }
            "copy" | "clone" => {
                want(name, args, 0)?;
                Ok(Value::Matrix(new_ref(self.clone())))
            }
            "save_npy" => {
                want(name, args, 1)?;
                let path = want_str(&args[0], "matrix.save_npy")?;
                save_npy_file(&path, &[self.rows, self.cols], &self.data)?;
                Ok(Value::Result(Ok(Box::new(Value::Null))))
            }
            _ => Err(unknown(name, "matrix")),
        }
    }
}

// -------------------------------------------------------------- ring_buffer

/// Fixed capacity. `push` overwrites the oldest entry when full.
#[derive(Debug, Default)]
pub struct RingData {
    pub cap: usize,
    pub items: VecDeque<Value>,
}

impl RingData {
    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.items.len() as i64))
            }
            "capacity" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.cap as i64))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.items.is_empty()))
            }
            "is_full" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.items.len() == self.cap))
            }
            "clear" => {
                want(name, args, 0)?;
                self.items.clear();
                Ok(Value::Null)
            }
            "push" => {
                want(name, args, 1)?;
                if self.items.len() == self.cap {
                    self.items.pop_front();
                }
                self.items.push_back(args[0].clone());
                Ok(Value::Null)
            }
            "peek" => {
                want(name, args, 0)?;
                match self.items.front() {
                    Some(v) => Ok(v.clone()),
                    None => Err("peek on an empty ring_buffer".to_string()),
                }
            }
            "pop" => {
                want(name, args, 0)?;
                match self.items.pop_front() {
                    Some(v) => Ok(v),
                    None => Err("pop on an empty ring_buffer".to_string()),
                }
            }
            "contains" => {
                want(name, args, 1)?;
                Ok(Value::Bool(
                    self.items.iter().any(|v| values_equal(v, &args[0])),
                ))
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::Ring(new_ref(RingData {
                    cap: self.cap,
                    items: self.items.iter().map(deep_clone).collect(),
                })))
            }
            _ => Err(unknown(name, "ring_buffer")),
        }
    }
}

// --------------------------------------------------------------------- bag

/// Multiset: counts per value. `len` is the total number of items.
#[derive(Debug, Default)]
pub struct BagData {
    pub counts: Vec<(Value, usize)>,
}

impl BagData {
    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "len" => {
                want(name, args, 0)?;
                Ok(Value::Int(self.counts.iter().map(|(_, n)| *n as i64).sum()))
            }
            "is_empty" => {
                want(name, args, 0)?;
                Ok(Value::Bool(self.counts.iter().all(|(_, n)| *n == 0)))
            }
            "clear" => {
                want(name, args, 0)?;
                self.counts.clear();
                Ok(Value::Null)
            }
            "push" | "add" => {
                want(name, args, 1)?;
                match self
                    .counts
                    .iter_mut()
                    .find(|(v, _)| values_equal(v, &args[0]))
                {
                    Some((_, n)) => *n += 1,
                    None => self.counts.push((args[0].clone(), 1)),
                }
                Ok(Value::Null)
            }
            "remove" => {
                want(name, args, 1)?;
                match self
                    .counts
                    .iter_mut()
                    .find(|(v, _)| values_equal(v, &args[0]))
                {
                    Some((_, n)) if *n > 0 => {
                        *n -= 1;
                        if *n == 0 {
                            self.counts
                                .retain(|(v, n)| *n > 0 || !values_equal(v, &args[0]));
                        }
                        Ok(Value::Null)
                    }
                    _ => Err("item not in bag".to_string()),
                }
            }
            "count_of" => {
                want(name, args, 1)?;
                let n = self
                    .counts
                    .iter()
                    .find(|(v, _)| values_equal(v, &args[0]))
                    .map(|(_, n)| *n)
                    .unwrap_or(0);
                Ok(Value::Int(n as i64))
            }
            "contains" => {
                want(name, args, 1)?;
                let has = self
                    .counts
                    .iter()
                    .any(|(v, n)| *n > 0 && values_equal(v, &args[0]));
                Ok(Value::Bool(has))
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::Bag(new_ref(BagData {
                    counts: self
                        .counts
                        .iter()
                        .map(|(v, n)| (deep_clone(v), *n))
                        .collect(),
                })))
            }
            _ => Err(unknown(name, "bag")),
        }
    }
}

// --------------------------------------------------------------------- ref

#[derive(Debug)]
pub struct RefBoxData {
    pub value: Value,
}

impl Default for RefBoxData {
    fn default() -> Self {
        RefBoxData { value: Value::Null }
    }
}

impl RefBoxData {
    pub fn method(&mut self, name: &str, args: &[Value]) -> Result<Value, String> {
        match name {
            "get" => {
                want(name, args, 0)?;
                Ok(self.value.clone())
            }
            "set" => {
                want(name, args, 1)?;
                self.value = args[0].clone();
                Ok(Value::Null)
            }
            _ => Err(unknown(name, "ref")),
        }
    }
}

/// Tuple: fixed arity, no push, index assignment fails.
pub fn tuple_method(items: &[Value], name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "len" => {
            want(name, args, 0)?;
            Ok(Value::Int(items.len() as i64))
        }
        "is_empty" => {
            want(name, args, 0)?;
            Ok(Value::Bool(items.is_empty()))
        }
        "get" => {
            want(name, args, 1)?;
            let i = want_index(&args[0], "tuple.get")?;
            match items.get(i) {
                Some(v) => Ok(v.clone()),
                None => Err(format!(
                    "index {} out of bounds for tuple of arity {}",
                    i,
                    items.len()
                )),
            }
        }
        "contains" => {
            want(name, args, 1)?;
            Ok(Value::Bool(items.iter().any(|v| values_equal(v, &args[0]))))
        }
        "clone" => {
            want(name, args, 0)?;
            Ok(Value::Tuple(new_ref(
                items.iter().map(deep_clone).collect(),
            )))
        }
        "push" | "pop" | "set" | "insert" => Err(format!("tuple is immutable: no {}", name)),
        _ => Err(unknown(name, "tuple")),
    }
}

/// opt-in helpers used by the interpreter for iteration and length.
pub fn iter_list(v: &Value) -> Result<Vec<Value>, String> {
    super::seq::vec_items(v)
}

pub fn parse_direction(s: &str) -> Result<bool, String> {
    match s {
        "min" => Ok(true),
        "max" => Ok(false),
        other => Err(format!("expected \"min\" or \"max\", found \"{}\"", other)),
    }
}

// ------------------------------------------------------------------ bytes helpers

pub fn resolve_bytes_index(idx: &Value, len: usize, who: &str) -> Result<usize, String> {
    let i = want_int(idx, who)?;
    if i >= 0 {
        let u = i as usize;
        if u < len {
            Ok(u)
        } else {
            Err(format!("index {} out of bounds for length {}", i, len))
        }
    } else {
        let neg = i.unsigned_abs() as usize;
        if neg <= len {
            Ok(len - neg)
        } else {
            Err(format!("index {} out of bounds for length {}", i, len))
        }
    }
}

pub fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{:02x}", b);
    }
    out
}

pub fn bytes_from_hex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err("hex string must have an even length".to_string());
    }
    if !s.is_ascii() {
        // Hex digits are ASCII by definition. Rejecting non-ASCII up front
        // also guarantees the byte-offset slicing below can never split a
        // multi-byte UTF-8 character (which would panic the host on inputs
        // like "a⚡", whose even byte length hides the mid-character cut).
        return Err("hex string must contain only ASCII characters".to_string());
    }
    let mut bytes = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16)
            .map_err(|_| format!("invalid hex sequence '{}'", &s[i..i + 2]))?;
        bytes.push(byte);
    }
    Ok(bytes)
}

const B64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn bytes_to_base64(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = if i + 1 < bytes.len() { bytes[i + 1] } else { 0 };
        let b2 = if i + 2 < bytes.len() { bytes[i + 2] } else { 0 };

        let idx0 = (b0 >> 2) as usize;
        let idx1 = (((b0 & 0x03) << 4) | (b1 >> 4)) as usize;
        let idx2 = (((b1 & 0x0f) << 2) | (b2 >> 6)) as usize;
        let idx3 = (b2 & 0x3f) as usize;

        out.push(B64_CHARS[idx0] as char);
        out.push(B64_CHARS[idx1] as char);
        if i + 1 < bytes.len() {
            out.push(B64_CHARS[idx2] as char);
        } else {
            out.push('=');
        }
        if i + 2 < bytes.len() {
            out.push(B64_CHARS[idx3] as char);
        } else {
            out.push('=');
        }
        i += 3;
    }
    out
}

pub fn bytes_from_base64(s: &str) -> Result<Vec<u8>, String> {
    fn b64_val(c: u8) -> Result<u8, String> {
        match c {
            b'A'..=b'Z' => Ok(c - b'A'),
            b'a'..=b'z' => Ok(c - b'a' + 26),
            b'0'..=b'9' => Ok(c - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(format!("invalid base64 character '{}'", c as char)),
        }
    }
    let clean: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if clean.is_empty() {
        return Ok(Vec::new());
    }
    if !clean.len().is_multiple_of(4) {
        return Err("invalid base64 length (must be a multiple of 4)".to_string());
    }
    let mut out = Vec::with_capacity((clean.len() / 4) * 3);
    for chunk in clean.as_chunks::<4>().0 {
        let v0 = b64_val(chunk[0])?;
        let v1 = b64_val(chunk[1])?;
        let v2 = if chunk[2] == b'=' { 0 } else { b64_val(chunk[2])? };
        let v3 = if chunk[3] == b'=' { 0 } else { b64_val(chunk[3])? };

        let b0 = (v0 << 2) | (v1 >> 4);
        out.push(b0);

        if chunk[2] != b'=' {
            let b1 = ((v1 & 0x0f) << 4) | (v2 >> 2);
            out.push(b1);
        } else if chunk[3] != b'=' {
            return Err("invalid base64 padding".to_string());
        }

        if chunk[3] != b'=' {
            let b2 = ((v2 & 0x03) << 6) | v3;
            out.push(b2);
        }
    }
    Ok(out)
}

pub fn bytes_method(cell: &Ref<Vec<u8>>, name: &str, args: &[Value]) -> Result<Value, String> {
    match name {
        "len" => {
            want(name, args, 0)?;
            Ok(Value::Int(cell.lock().unwrap().len() as i64))
        }
        "is_empty" => {
            want(name, args, 0)?;
            Ok(Value::Bool(cell.lock().unwrap().is_empty()))
        }
        "get" => {
            want(name, args, 1)?;
            let b = cell.lock().unwrap();
            let i = resolve_bytes_index(&args[0], b.len(), "bytes.get")?;
            Ok(Value::Int(b[i] as i64))
        }
        "set" => {
            want(name, args, 2)?;
            let mut b = cell.lock().unwrap();
            let i = resolve_bytes_index(&args[0], b.len(), "bytes.set")?;
            let val = want_int(&args[1], "bytes.set")?;
            if !(0..=255).contains(&val) {
                return Err(format!("byte value must be between 0 and 255, got {}", val));
            }
            b[i] = val as u8;
            Ok(Value::Null)
        }
        "push" => {
            want(name, args, 1)?;
            let mut b = cell.lock().unwrap();
            let val = want_int(&args[0], "bytes.push")?;
            if !(0..=255).contains(&val) {
                return Err(format!("byte value must be between 0 and 255, got {}", val));
            }
            b.push(val as u8);
            Ok(Value::Null)
        }
        "slice" => {
            if args.is_empty() || args.len() > 2 {
                return Err(format!("bytes.slice expects 1 or 2 arguments, got {}", args.len()));
            }
            let b = cell.lock().unwrap();
            let len = b.len();
            let s = resolve_slice_bound(&args[0], len, "bytes.slice")?;
            let e = if args.len() == 2 {
                resolve_slice_bound(&args[1], len, "bytes.slice")?
            } else {
                len
            };
            let sub = if s < e { b[s..e].to_vec() } else { Vec::new() };
            Ok(Value::Bytes(new_ref(sub)))
        }
        "to_list" => {
            want(name, args, 0)?;
            let b = cell.lock().unwrap();
            let items: Vec<Value> = b.iter().map(|&x| Value::Int(x as i64)).collect();
            Ok(make_list(items))
        }
        "to_string" => {
            want(name, args, 0)?;
            let b = cell.lock().unwrap();
            match std::str::from_utf8(&b) {
                Ok(s) => Ok(Value::Result(Ok(Box::new(Value::Str(s.into()))))),
                Err(e) => Ok(Value::Result(Err(Box::new(Value::Str(format!("utf-8 error: {}", e).into()))))),
            }
        }
        "to_hex" => {
            want(name, args, 0)?;
            let b = cell.lock().unwrap();
            Ok(Value::Str(bytes_to_hex(&b).into()))
        }
        "from_hex" => {
            want(name, args, 1)?;
            let s = want_str(&args[0], "bytes.from_hex")?;
            let data = bytes_from_hex(&s)?;
            Ok(Value::Bytes(new_ref(data)))
        }
        "to_base64" => {
            want(name, args, 0)?;
            let b = cell.lock().unwrap();
            Ok(Value::Str(bytes_to_base64(&b).into()))
        }
        "from_base64" => {
            want(name, args, 1)?;
            let s = want_str(&args[0], "bytes.from_base64")?;
            let data = bytes_from_base64(&s)?;
            Ok(Value::Bytes(new_ref(data)))
        }
        "copy" | "clone" => {
            want(name, args, 0)?;
            let data = cell.lock().unwrap().clone();
            Ok(Value::Bytes(new_ref(data)))
        }
        _ => Err(unknown(name, "bytes")),
    }
}

// ------------------------------------------------------------------ npy format

pub enum NpyLoaded {
    Vector(Vec<f64>),
    Matrix { rows: usize, cols: usize, data: Vec<f64> },
}

pub fn save_npy_file(path: &str, shape: &[usize], data: &[f64]) -> Result<(), String> {
    let shape_str = if shape.len() == 1 {
        format!("({},), ", shape[0])
    } else if shape.len() == 2 {
        format!("({}, {}), ", shape[0], shape[1])
    } else {
        return Err("npy save only supports 1D and 2D arrays".to_string());
    };

    let dict = format!("{{'descr': '<f8', 'fortran_order': False, 'shape': {}}}", shape_str);
    let unpadded_len = 10 + dict.len() + 1; // +1 for trailing '\n'
    let pad = (64 - (unpadded_len % 64)) % 64;
    let mut header = dict;
    header.push_str(&" ".repeat(pad));
    header.push('\n');

    let h_len = header.len();
    if h_len > u16::MAX as usize {
        return Err("npy header too large".to_string());
    }

    let mut out = Vec::with_capacity(10 + h_len + data.len() * 8);
    out.extend_from_slice(b"\x93NUMPY\x01\x00");
    out.extend_from_slice(&(h_len as u16).to_le_bytes());
    out.extend_from_slice(header.as_bytes());
    for &val in data {
        out.extend_from_slice(&val.to_le_bytes());
    }

    std::fs::write(path, out).map_err(|e| format!("cannot write npy file '{}': {}", path, e))
}

pub fn load_npy_data(path: &str) -> Result<NpyLoaded, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read npy file '{}': {}", path, e))?;
    if bytes.len() < 10 {
        return Err("npy file is too short (truncated header)".to_string());
    }
    if &bytes[0..6] != b"\x93NUMPY" {
        return Err("invalid npy file (bad magic number)".to_string());
    }
    if bytes[6] != 1 || bytes[7] != 0 {
        return Err(format!("unsupported npy version {}.{}", bytes[6], bytes[7]));
    }
    let h_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
    if 10 + h_len > bytes.len() {
        return Err("truncated npy header".to_string());
    }
    let h_str = std::str::from_utf8(&bytes[10..10 + h_len])
        .map_err(|_| "npy header is not valid utf-8/ascii".to_string())?;

    if !h_str.contains("'<f8'") && !h_str.contains("\"<f8\"") && !h_str.contains("'|f8'") && !h_str.contains("\"|f8\"") {
        return Err(format!("unsupported npy dtype in header: {}", h_str));
    }

    let open_paren = h_str.find('(').ok_or_else(|| "missing shape in npy header".to_string())?;
    let close_paren = h_str[open_paren..]
        .find(')')
        .ok_or_else(|| "unterminated shape in npy header".to_string())?
        + open_paren;
    let shape_str = &h_str[open_paren + 1..close_paren];
    let dims: Vec<usize> = shape_str
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<usize>().map_err(|_| format!("invalid dimension '{}'", s)))
        .collect::<Result<Vec<_>, _>>()?;

    let payload = &bytes[10 + h_len..];
    if dims.len() == 1 {
        let n = dims[0];
        if payload.len() != n * 8 {
            return Err(format!(
                "npy payload size mismatch: expected {} bytes, got {}",
                n * 8,
                payload.len()
            ));
        }
        let mut data = Vec::with_capacity(n);
        for chunk in payload.as_chunks::<8>().0 {
            data.push(f64::from_le_bytes(*chunk));
        }
        Ok(NpyLoaded::Vector(data))
    } else if dims.len() == 2 {
        let (rows, cols) = (dims[0], dims[1]);
        let total = rows * cols;
        if payload.len() != total * 8 {
            return Err(format!(
                "npy payload size mismatch: expected {} bytes, got {}",
                total * 8,
                payload.len()
            ));
        }
        let mut data = Vec::with_capacity(total);
        for chunk in payload.as_chunks::<8>().0 {
            data.push(f64::from_le_bytes(*chunk));
        }
        Ok(NpyLoaded::Matrix { rows, cols, data })
    } else {
        Err(format!(
            "only 1D vector and 2D matrix npy files are supported, found {}D shape",
            dims.len()
        ))
    }
}


// ------------------------------------------------------- detached operations

/// Detached bag operations (audit A-7): `add`/`remove`/`count_of`/`contains`
/// compare item values with values_equal, and an item can be (or reach) the
/// bag itself — running them under the receiver's Mutex would re-lock it and
/// hang. The dispatcher snapshots `counts` with the lock released before
/// calling here; `d` is only re-locked for the tiny structural commit (no
/// value traversal under the lock).
pub fn bag_detached(
    d: &crate::value::Ref<BagData>,
    counts: Vec<(Value, usize)>,
    name: &str,
    args: &[Value],
) -> Result<Value, String> {
    match name {
        "push" | "add" => {
            want(name, args, 1)?;
            match counts.iter().position(|(v, _)| values_equal(v, &args[0])) {
                Some(i) => {
                    let mut g = d.lock().unwrap();
                    if i < g.counts.len() {
                        g.counts[i].1 += 1;
                    } else {
                        g.counts.push((args[0].clone(), 1));
                    }
                    Ok(Value::Null)
                }
                None => {
                    d.lock().unwrap().counts.push((args[0].clone(), 1));
                    Ok(Value::Null)
                }
            }
        }
        "remove" => {
            want(name, args, 1)?;
            match counts.iter().position(|(v, _)| values_equal(v, &args[0])) {
                Some(i) => {
                    let mut g = d.lock().unwrap();
                    // Defensive: a zero count would underflow the usize
                    // (a host panic from script input). Mirrors the
                    // original `*n > 0` guard in `BagData::method`. The
                    // same-slot check (no locking) keeps a concurrent
                    // mutation from decrementing a different item.
                    match g.counts.get(i) {
                        Some((cur, n)) if *n > 0 && crate::value::same_slot(cur, &counts[i].0) => {
                            g.counts[i].1 -= 1;
                            if g.counts[i].1 == 0 {
                                g.counts.remove(i);
                            }
                            Ok(Value::Null)
                        }
                        _ => Err("item not in bag".to_string()),
                    }
                }
                None => Err("item not in bag".to_string()),
            }
        }
        "count_of" => {
            want(name, args, 1)?;
            let n = counts
                .iter()
                .find(|(v, _)| values_equal(v, &args[0]))
                .map(|(_, n)| *n)
                .unwrap_or(0);
            Ok(Value::Int(n as i64))
        }
        "contains" => {
            want(name, args, 1)?;
            let has = counts
                .iter()
                .any(|(v, n)| *n > 0 && values_equal(v, &args[0]));
            Ok(Value::Bool(has))
        }
        _ => unreachable!(),
    }
}

/// Detached graph operations (audit A-7): node resolution is values_equal
/// over node values, which can be (or reach) the graph itself — running it
/// under the receiver's Mutex would re-lock it and hang. Reads run on a
/// shallow clone with no lock held; mutations resolve indices on the
/// snapshot, then commit by index under the lock (no value traversal).
pub fn graph_detached(
    d: &crate::value::Ref<GraphData>,
    name: &str,
    args: &[Value],
) -> Result<Value, String> {
    match name {
        "add_node" => {
            want(name, args, 1)?;
            let exists = {
                let g = d.lock().unwrap().clone();
                g.index_of(&args[0]).is_some()
            };
            if exists {
                return Err(format!("node already exists: {}", short(&args[0])));
            }
            let mut g = d.lock().unwrap();
            g.nodes.push(args[0].clone());
            g.adj.push(Vec::new());
            Ok(Value::Null)
        }
        "add_edge" => {
            if args.len() < 2 || args.len() > 3 {
                return Err(format!(
                    "add_edge expects 2 or 3 arguments, got {}",
                    args.len()
                ));
            }
            let w = if args.len() == 3 {
                want_num(&args[2], "add_edge")?
            } else {
                1.0
            };
            let (ia, ib) = {
                let g = d.lock().unwrap().clone();
                (g.index_of(&args[0]), g.index_of(&args[1]))
            };
            let ia = ia
                .ok_or_else(|| format!("edge endpoint is not a node: {}", short(&args[0])))?;
            let ib = ib
                .ok_or_else(|| format!("edge endpoint is not a node: {}", short(&args[1])))?;
            let mut g = d.lock().unwrap();
            if ia >= g.adj.len() || ib >= g.nodes.len() {
                return Err("edge endpoint is not a node".to_string());
            }
            g.adj[ia].push(Edge { to: ib, weight: w });
            if !g.directed {
                g.adj[ib].push(Edge { to: ia, weight: w });
            }
            Ok(Value::Null)
        }
        // Pure reads: the snapshot shares node Values by Arc, and with the
        // receiver's Mutex released, values_equal may re-lock it freely.
        other => {
            let mut g = d.lock().unwrap().clone();
            g.method(other, args)
        }
    }
}
