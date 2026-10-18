//! Remaining structures: heap, priority_queue (stable ties), graph, hash_table
//! (visible buckets and rehash), trie, matrix, ring_buffer, bag, ref.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use super::nodes::join_words;
use super::seq::{make_list, unknown, value_cmp, want, want_index, want_num, want_str};
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
#[derive(Debug, Default)]
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

// ------------------------------------------------------------------ matrix

/// Row-major numeric grid. `add` and `mul` check dimensions.
#[derive(Debug, Clone)]
pub struct MatrixData {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<f64>,
}

impl MatrixData {
    pub fn new(rows: usize, cols: usize, fill: f64) -> Self {
        MatrixData {
            rows,
            cols,
            data: vec![fill; rows * cols],
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

    pub fn mul(&self, other: &MatrixData) -> Result<Value, String> {
        if self.cols != other.rows {
            return Err(format!(
                "matrix mul needs {} cols to match {} rows",
                self.cols, other.rows
            ));
        }
        let mut data = vec![0.0; self.rows * other.cols];
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
            "add" | "mul" => {
                want(name, args, 1)?;
                let other = match &args[0] {
                    Value::Matrix(m) => m.lock().unwrap().clone(),
                    o => return Err(format!("expected a matrix, found {}", super::type_name(o))),
                };
                if name == "add" {
                    self.add(&other)
                } else {
                    self.mul(&other)
                }
            }
            "clone" => {
                want(name, args, 0)?;
                Ok(Value::Matrix(new_ref(self.clone())))
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
