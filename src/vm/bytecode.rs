//! Bytecode for the Spid3r virtual machine.
//!
//! A `Chunk` is one compiled function body (or the top-level script). Each
//! `Chunk` owns its constant pool, a pool of nested compiled functions, and a
//! pool of class prototypes. Instruction operands are little-endian and sit
//! immediately after the one-byte opcode. `OpTry` carries two *relative*
//! `i16` offsets (to the catch block and to the finally block) so it can be
//! patched before the targets exist.

use std::sync::Arc;

use crate::ast::Vis;
use crate::diagnostics::{err, Phase, Span, SpidResult};
use crate::value::{Value, VmFunc};

/// One compiled function or script body.
#[derive(Clone, Debug)]
pub struct Chunk {
    pub code: Vec<u8>,
    pub constants: Vec<Value>,
    /// Nested function prototypes. Stored as `Arc` so creating a closure at
    /// run time is a cheap `Arc` clone instead of a deep copy of the whole
    /// bytecode (code, constants, and per-instruction spans).
    pub functions: Vec<std::sync::Arc<VmFuncProto>>,
    pub classes: Vec<VmClassProto>,
    /// `inst_offsets[i]` is the byte offset where instruction `i` starts;
    /// `inst_spans[i]` is its source span. Used to attribute runtime errors.
    pub inst_offsets: Vec<usize>,
    pub inst_spans: Vec<Span>,
    /// Span to use for the next emitted opcode.
    pub pending_span: Span,
    /// Modules embedded by the compiler so `exec` needs no source. Keyed by the
    /// import literal; flattened transitively so every dependency is present.
    pub modules: Vec<EmbeddedModule>,
}

/// A compiled function as stored in a `Chunk`'s function pool.
#[derive(Clone, Debug)]
pub struct VmFuncProto {
    pub name: Option<String>,
    pub arity: usize,
    pub is_async: bool,
    pub upvalues: Vec<u8>,
    /// How many upvalue cells this function allocates for its own captured locals.
    pub own_upvalues: u8,
    /// Shared: cloning a prototype (closure creation) is an `Arc` bump.
    pub chunk: std::sync::Arc<Chunk>,
}

/// A class as stored in a `Chunk`'s class pool. The runtime `VmClass` is built
/// from this when the `OpClass` executes, resolving the parent by name and
/// lifting method prototypes into `VmFunc` values.
#[derive(Clone, Debug)]
pub struct VmClassProto {
    pub name: String,
    pub parent: Option<String>,
    pub fields: Vec<(String, Vis)>,
    /// (method name, visibility, function-pool index, is_constructor)
    pub methods: Vec<(String, Vis, u16, bool)>,
}

/// Magic number at the start of every `.matrixc` file.
pub const FORMAT_MAGIC: &[u8; 6] = b"SPID3R";
/// On-disk bytecode format version. Bump when the layout changes incompatibly.
pub const FORMAT_VERSION: u16 = 1;

/// A module compiled and embedded into a `.matrixc` file so that `exec` can run
/// the program without re-reading any `.matrix` source. `key` is the import
/// literal used to reference the module (`import "key"`); `public` is the set of
/// exported top-level names; `func` is the module's compiled script function.
#[derive(Clone)]
pub struct EmbeddedModule {
    pub key: String,
    pub public: Vec<String>,
    pub func: Arc<VmFunc>,
}

impl std::fmt::Debug for EmbeddedModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddedModule")
            .field("key", &self.key)
            .field("public", &self.public)
            .field("func", &"<VmFunc>")
            .finish()
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpCode {
    Constant = 0,
    Null = 1,
    True = 2,
    False = 3,
    None = 4,
    Pop = 5,
    GetLocal = 6,
    SetLocal = 7,
    GetGlobal = 8,
    SetGlobal = 9,
    DefineGlobal = 10,
    GetUpvalue = 11,
    SetUpvalue = 12,
    Closure = 13,
    Add = 14,
    Sub = 15,
    Mul = 16,
    Div = 17,
    Mod = 18,
    Neg = 19,
    Not = 20,
    Equal = 21,
    NotEqual = 22,
    Less = 23,
    LessEq = 24,
    Greater = 25,
    GreaterEq = 26,
    Jump = 27,
    JumpIfFalse = 28,
    JumpIfTrue = 29,
    Loop = 30,
    Call = 31,
    Invoke = 32,
    IndexGet = 33,
    IndexSet = 34,
    FieldGet = 35,
    FieldSet = 36,
    Construct = 37,
    Return = 38,
    Try = 39,
    EndTry = 40,
    Throw = 41,
    Await = 42,
    Spawn = 43,
    Import = 44,
    Class = 45,
    /// Pop the top value and push `Bool(truthy(top))` (logical `&&`/`||`).
    Truthy = 46,
    /// Pop a collection/iterable and push a `tuple` of its materialized items
    /// (used by the `for-in` desugaring).
    ForEachInit = 47,
    /// Pop a value and push its length as an `int` (mirrors `len()`).
    Len = 48,
    /// Mark the top of the handler stack as being inside its `finally` block,
    /// so a throw from `finally` re-raises instead of re-running it.
    EnterFinally = 49,
    /// Duplicate the top stack value (`a` -> `a a`). Used by compound
    /// assignment to field/index targets so the object reference survives the
    /// `FieldGet`/`IndexGet` that would otherwise consume it.
    Dup = 50,
    /// Duplicate the top two stack values (`a b` -> `a b a b`). Same purpose
    /// for compound assignment on index targets (`obj[idx] += v`).
    Dup2 = 51,
    /// Bind a captured local to its cell at closure-creation time:
    /// operands are `(local_slot, own_upvalue_index)`. Copies the local's
    /// current stack value into the frame's own upvalue cell AND registers
    /// `captured[slot] = cell`, so every later `GetLocal`/`SetLocal` of that
    /// slot reads/writes the same cell the capturing closure shares. Without
    /// this, code emitted before the capture (e.g. a loop condition compiled
    /// with `GetLocal`) reads a stale slot forever while the body writes the
    /// cell - the two storage locations diverge.
    CaptureSync = 52,
    /// Release the captured-slot routing for one local (`slot` u8 operand):
    /// removes `slot` from the frame's `captured` map so the raw stack slot is
    /// the storage again. Emitted when a scope containing a captured local
    /// exits, and at the top of each `for x in …` iteration so every iteration
    /// re-captures a fresh cell (matching the tree interpreter's fresh
    /// binding per iteration). No-op when the slot is not currently captured.
    Uncapture = 53,
    ImportAs = 54,
}

impl OpCode {
    /// Safe decode of a raw byte into an `OpCode`.
    ///
    /// Returns `None` for any byte that is not a valid opcode, so callers can
    /// raise a clean "invalid opcode" error instead of invoking the undefined
    /// behaviour that an unchecked `mem::transmute` would produce on an invalid
    /// tag. The match mirrors the `#[repr(u8)]` discriminants in the enum.
    pub fn from_u8(b: u8) -> Option<OpCode> {
        match b {
            0 => Some(OpCode::Constant),
            1 => Some(OpCode::Null),
            2 => Some(OpCode::True),
            3 => Some(OpCode::False),
            4 => Some(OpCode::None),
            5 => Some(OpCode::Pop),
            6 => Some(OpCode::GetLocal),
            7 => Some(OpCode::SetLocal),
            8 => Some(OpCode::GetGlobal),
            9 => Some(OpCode::SetGlobal),
            10 => Some(OpCode::DefineGlobal),
            11 => Some(OpCode::GetUpvalue),
            12 => Some(OpCode::SetUpvalue),
            13 => Some(OpCode::Closure),
            14 => Some(OpCode::Add),
            15 => Some(OpCode::Sub),
            16 => Some(OpCode::Mul),
            17 => Some(OpCode::Div),
            18 => Some(OpCode::Mod),
            19 => Some(OpCode::Neg),
            20 => Some(OpCode::Not),
            21 => Some(OpCode::Equal),
            22 => Some(OpCode::NotEqual),
            23 => Some(OpCode::Less),
            24 => Some(OpCode::LessEq),
            25 => Some(OpCode::Greater),
            26 => Some(OpCode::GreaterEq),
            27 => Some(OpCode::Jump),
            28 => Some(OpCode::JumpIfFalse),
            29 => Some(OpCode::JumpIfTrue),
            30 => Some(OpCode::Loop),
            31 => Some(OpCode::Call),
            32 => Some(OpCode::Invoke),
            33 => Some(OpCode::IndexGet),
            34 => Some(OpCode::IndexSet),
            35 => Some(OpCode::FieldGet),
            36 => Some(OpCode::FieldSet),
            37 => Some(OpCode::Construct),
            38 => Some(OpCode::Return),
            39 => Some(OpCode::Try),
            40 => Some(OpCode::EndTry),
            41 => Some(OpCode::Throw),
            42 => Some(OpCode::Await),
            43 => Some(OpCode::Spawn),
            44 => Some(OpCode::Import),
            45 => Some(OpCode::Class),
            46 => Some(OpCode::Truthy),
            47 => Some(OpCode::ForEachInit),
            48 => Some(OpCode::Len),
            49 => Some(OpCode::EnterFinally),
            50 => Some(OpCode::Dup),
            51 => Some(OpCode::Dup2),
            52 => Some(OpCode::CaptureSync),
            53 => Some(OpCode::Uncapture),
            54 => Some(OpCode::ImportAs),
            _ => None,
        }
    }
}

impl Default for Chunk {
    fn default() -> Self {
        Self::new()
    }
}

impl Chunk {
    pub fn new() -> Self {
        Chunk {
            code: Vec::new(),
            constants: Vec::new(),
            functions: Vec::new(),
            classes: Vec::new(),
            inst_offsets: Vec::new(),
            inst_spans: Vec::new(),
            pending_span: Span::new(Arc::from("<spid3r>"), 0, 0, 0),
            modules: Vec::new(),
        }
    }

    pub fn set_span(&mut self, span: Span) {
        self.pending_span = span;
    }

    fn emit_opcode(&mut self, op: OpCode) {
        let off = self.code.len();
        self.inst_offsets.push(off);
        self.inst_spans.push(self.pending_span.clone());
        self.code.push(op as u8);
    }

    pub fn emit(&mut self, op: OpCode) {
        self.emit_opcode(op);
    }

    pub fn emit_u8(&mut self, op: OpCode, v: u8) {
        self.emit_opcode(op);
        self.code.push(v);
    }

    pub fn emit_u16(&mut self, op: OpCode, v: u16) {
        self.emit_opcode(op);
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    /// Emit one opcode followed by a `u16` operand and a `u8` operand. Used by
    /// `Construct` and `Invoke` (name/field constant index plus argument count).
    pub fn emit_u16_u8(&mut self, op: OpCode, a: u16, b: u8) {
        self.emit_opcode(op);
        self.code.extend_from_slice(&a.to_le_bytes());
        self.code.push(b);
    }

    /// Emit one opcode followed by two `u16` operands (e.g. `ImportAs`).
    pub fn emit_u16_u16(&mut self, op: OpCode, a: u16, b: u16) {
        self.emit_opcode(op);
        self.code.extend_from_slice(&a.to_le_bytes());
        self.code.extend_from_slice(&b.to_le_bytes());
    }

    /// Emit one opcode followed by two `u8` operands (e.g. `CaptureSync`).
    pub fn emit_u8_u8(&mut self, op: OpCode, a: u8, b: u8) {
        self.emit_opcode(op);
        self.code.push(a);
        self.code.push(b);
    }

    pub fn emit_i16(&mut self, op: OpCode, v: i16) {
        self.emit_opcode(op);
        self.code.extend_from_slice(&v.to_le_bytes());
    }

    pub fn emit_const(&mut self, v: Value) -> u16 {
        // Reuse identical constants so equality/identity stays stable.
        if let Some(i) = self.constants.iter().position(|c| const_eq(c, &v)) {
            return i as u16;
        }
        let idx = self.constants.len() as u16;
        self.constants.push(v);
        idx
    }

    pub fn emit_str_const(&mut self, s: &str) -> u16 {
        self.emit_const(Value::Str(s.into()))
    }

    pub fn add_function(&mut self, f: VmFuncProto) -> u16 {
        let idx = self.functions.len() as u16;
        self.functions.push(std::sync::Arc::new(f));
        idx
    }

    pub fn add_class(&mut self, c: VmClassProto) -> u16 {
        let idx = self.classes.len() as u16;
        self.classes.push(c);
        idx
    }

    /// Byte offset just past the last emitted byte; used as a patch target.
    pub fn here(&self) -> usize {
        self.code.len()
    }

    /// Patch a 2-byte little-endian operand at `pos` (the position of the first
    /// operand byte) with `val`.
    pub fn patch_u16(&mut self, pos: usize, val: u16) {
        let bytes = val.to_le_bytes();
        self.code[pos] = bytes[0];
        self.code[pos + 1] = bytes[1];
    }

    pub fn patch_i16(&mut self, pos: usize, val: i16) {
        let bytes = val.to_le_bytes();
        self.code[pos] = bytes[0];
        self.code[pos + 1] = bytes[1];
    }

    /// `OpTry`: `has_catch` flags whether a `catch` block exists; `catch_slot`
    /// is the local slot that receives the thrown value; `catch_off` and
    /// `finally_off` are relative `i16` offsets (from the byte after the
    /// operands) to the catch block and the finally block. When there is no
    /// catch, `catch_off == finally_off`.
    pub fn emit_try(&mut self, has_catch: bool, catch_slot: u8, catch_off: i16, finally_off: i16) {
        self.emit_opcode(OpCode::Try);
        self.code.push(has_catch as u8);
        self.code.push(catch_slot);
        self.code.extend_from_slice(&catch_off.to_le_bytes());
        self.code.extend_from_slice(&finally_off.to_le_bytes());
    }

    /// Span for the instruction containing byte offset `ip`.
    ///
    /// Defensive by construction: `inst_spans` is indexed through `get` and
    /// falls back to `pending_span`. A crafted `.matrixc` can carry an
    /// `inst_offsets` table longer than `inst_spans` (the deserializer reads the
    /// two counts independently), and the old blind `inst_spans[lo - 1]` index
    /// panicked the host on exactly that file (audit A-8).
    pub fn span_at(&self, ip: usize) -> Span {
        // `inst_offsets` is sorted; find the greatest start <= ip.
        let mut lo = 0usize;
        let mut hi = self.inst_offsets.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if self.inst_offsets[mid] <= ip {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo == 0 {
            return self.pending_span.clone();
        }
        self.inst_spans
            .get(lo - 1)
            .cloned()
            .unwrap_or_else(|| self.pending_span.clone())
    }
}

fn const_eq(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (Int(x), Int(y)) => x == y,
        (Float(x), Float(y)) => x == y,
        (Bool(x), Bool(y)) => x == y,
        (Str(x), Str(y)) => x == y,
        (Null, Null) => true,
        (Option(None), Option(None)) => true,
        _ => false,
    }
}

// ------------------------------------------------------------------- decode

/// Read one opcode starting at `ip`; returns the opcode and the offset of the
/// first operand byte.
pub fn read_op(code: &[u8], ip: usize) -> (OpCode, usize) {
    let byte = code[ip];
    match OpCode::from_u8(byte) {
        Some(op) => (op, ip + 1),
        // Malformed chunks shouldn't reach disassembly, but never invoke
        // undefined behaviour on a bad tag; treat as a no-op for best effort.
        None => (OpCode::Pop, ip + 1),
    }
}

pub fn read_u8(code: &[u8], ip: usize) -> (u8, usize) {
    (code[ip], ip + 1)
}

pub fn read_u16(code: &[u8], ip: usize) -> (u16, usize) {
    let v = u16::from_le_bytes([code[ip], code[ip + 1]]);
    (v, ip + 2)
}

pub fn read_i16(code: &[u8], ip: usize) -> (i16, usize) {
    let v = i16::from_le_bytes([code[ip], code[ip + 1]]);
    (v, ip + 2)
}

// ----------------------------------------------------------------- disasm

/// Human-readable disassembly, used by `compile --dump` and debugging.
/// Recurses into nested function and class prototypes.
pub fn disassemble(chunk: &Chunk, name: &str) -> String {
    let mut out = String::new();
    disassemble_into(chunk, name, &mut out, 0);
    out
}

fn disassemble_into(chunk: &Chunk, name: &str, out: &mut String, depth: usize) {
    let indent = "  ".repeat(depth);
    out.push_str(&format!(
        "{}== {} ({} bytes) ==\n",
        indent, name, chunk.code.len()
    ));
    let mut ip = 0usize;
    while ip < chunk.code.len() {
        let start = ip;
        let (op, mut o) = read_op(&chunk.code, ip);
        out.push_str(&format!("{}  {:04}: ", indent, start));
        match op {
            OpCode::Constant => {
                let (c, no) = read_u16(&chunk.code, o);
                o = no;
                out.push_str(&format!(
                    "CONSTANT {} ({})",
                    c,
                    display_const(&chunk.constants[c as usize])
                ));
            }
            OpCode::GetLocal | OpCode::SetLocal | OpCode::GetUpvalue | OpCode::SetUpvalue => {
                let (s, no) = read_u8(&chunk.code, o);
                o = no;
                out.push_str(&format!("{:?} slot {}", op, s));
            }
            OpCode::CaptureSync => {
                let (s, no) = read_u8(&chunk.code, o);
                let (u, no2) = read_u8(&chunk.code, no);
                o = no2;
                out.push_str(&format!("{:?} slot {} cell {}", op, s, u));
            }
            OpCode::Uncapture => {
                let (s, no) = read_u8(&chunk.code, o);
                o = no;
                out.push_str(&format!("{:?} slot {}", op, s));
            }
            OpCode::GetGlobal | OpCode::SetGlobal | OpCode::DefineGlobal | OpCode::Closure
            | OpCode::FieldGet | OpCode::FieldSet | OpCode::Import | OpCode::Class => {
                let (c, no) = read_u16(&chunk.code, o);
                o = no;
                out.push_str(&format!("{:?} {}", op, c));
            }
            OpCode::ImportAs => {
                let (c1, no) = read_u16(&chunk.code, o);
                let (c2, no2) = read_u16(&chunk.code, no);
                o = no2;
                out.push_str(&format!("{:?} {} alias {}", op, c1, c2));
            }
            OpCode::Construct | OpCode::Invoke => {
                let (c, no) = read_u16(&chunk.code, o);
                let (a, no2) = read_u8(&chunk.code, no);
                o = no2;
                out.push_str(&format!("{:?} {} argc {}", op, c, a));
            }
            OpCode::Jump | OpCode::JumpIfFalse | OpCode::JumpIfTrue | OpCode::Loop => {
                let (j, no) = read_i16(&chunk.code, o);
                o = no;
                out.push_str(&format!("{:?} {}", op, j));
            }
            OpCode::Call => {
                let (a, no) = read_u8(&chunk.code, o);
                o = no;
                out.push_str(&format!("{:?} argc {}", op, a));
            }
            _ => out.push_str(&format!("{:?}", op)),
        }
        out.push('\n');
        ip = o;
    }
    for (i, f) in chunk.functions.iter().enumerate() {
        let sub = format!("{} fn#{}", name, i);
        disassemble_into(&f.chunk, &sub, out, depth + 1);
    }
    for (i, c) in chunk.classes.iter().enumerate() {
        for (mname, _vis, fidx, is_ctor) in &c.methods {
            let tag = if *is_ctor { " (ctor)" } else { "" };
            let sub = format!("{} class#{}.{}{}", name, i, mname, tag);
            disassemble_into(&chunk.functions[*fidx as usize].chunk, &sub, out, depth + 1);
        }
    }
}

fn display_const(v: &Value) -> String {
    match v {
        Value::Str(s) => format!("\"{}\"", s),
        other => crate::value::display_value(other),
    }
}

// ----------------------------------------------------------------- serialize
//
// Compact little-endian binary format for a compiled script `VmFunc`. Used by
// `compile` to write a `.matrixc` file and by `exec` to load and run it on the
// VM. Only `Int`/`Float`/`Str` may appear as constants (the compiler never
// emits other values into the constant pool), so the value codec is tiny.

fn put_u8(b: &mut Vec<u8>, v: u8) {
    b.push(v);
}

fn put_u16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_usize(b: &mut Vec<u8>, v: usize) {
    b.extend_from_slice(&(v as u64).to_le_bytes());
}

fn put_bool(b: &mut Vec<u8>, v: bool) {
    b.push(if v { 1 } else { 0 });
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    put_usize(b, s.len());
    b.extend_from_slice(s.as_bytes());
}

fn put_opt_str(b: &mut Vec<u8>, o: &Option<String>) {
    match o {
        Some(s) => {
            b.push(1);
            put_str(b, s);
        }
        None => b.push(0),
    }
}

fn put_vis(b: &mut Vec<u8>, v: Vis) {
    b.push(match v {
        Vis::Private => 0,
        Vis::Public => 1,
    });
}

fn put_span(b: &mut Vec<u8>, s: &Span) {
    put_str(b, &s.file);
    put_usize(b, s.line as usize);
    put_usize(b, s.col as usize);
    put_usize(b, s.len as usize);
}

fn put_value(b: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Int(i) => {
            b.push(0);
            b.extend_from_slice(&(*i as u64).to_le_bytes());
        }
        Value::Float(f) => {
            b.push(1);
            b.extend_from_slice(&f.to_bits().to_le_bytes());
        }
        Value::Str(s) => {
            b.push(2);
            put_str(b, s);
        }
        other => panic!("bytecode: cannot serialize constant {:?}", other),
    }
}

fn put_chunk(b: &mut Vec<u8>, c: &Chunk) {
    put_usize(b, c.code.len());
    b.extend_from_slice(&c.code);
    put_usize(b, c.constants.len());
    for v in &c.constants {
        put_value(b, v);
    }
    put_usize(b, c.functions.len());
    for f in &c.functions {
        put_func_proto(b, f);
    }
    put_usize(b, c.classes.len());
    for cl in &c.classes {
        put_class_proto(b, cl);
    }
    put_usize(b, c.inst_offsets.len());
    for o in &c.inst_offsets {
        put_usize(b, *o);
    }
    put_usize(b, c.inst_spans.len());
    for s in &c.inst_spans {
        put_span(b, s);
    }
    put_span(b, &c.pending_span);
    put_usize(b, c.modules.len());
    for m in &c.modules {
        put_embedded(b, m);
    }
}

fn put_embedded(b: &mut Vec<u8>, m: &EmbeddedModule) {
    put_str(b, &m.key);
    put_usize(b, m.public.len());
    for n in &m.public {
        put_str(b, n);
    }
    put_vmfunc(b, &m.func);
}

/// Serialize a single `VmFunc` (used for the top-level script and for every
/// embedded module) into the buffer.
fn put_vmfunc(b: &mut Vec<u8>, func: &VmFunc) {
    put_opt_str(b, &func.name);
    put_usize(b, func.arity);
    put_bool(b, func.is_async);
    put_usize(b, func.upvalues.len());
    for cell in &func.upvalues {
        let v = cell.lock().unwrap().clone();
        put_value(b, &v);
    }
    put_u8(b, func.own_upvalues);
    put_chunk(b, &func.chunk);
}

fn put_func_proto(b: &mut Vec<u8>, f: &VmFuncProto) {
    put_opt_str(b, &f.name);
    put_usize(b, f.arity);
    put_bool(b, f.is_async);
    put_usize(b, f.upvalues.len());
    for u in &f.upvalues {
        b.push(*u);
    }
    put_u8(b, f.own_upvalues);
    put_chunk(b, &f.chunk);
}

fn put_class_proto(b: &mut Vec<u8>, c: &VmClassProto) {
    put_str(b, &c.name);
    put_opt_str(b, &c.parent);
    put_usize(b, c.fields.len());
    for (n, v) in &c.fields {
        put_str(b, n);
        put_vis(b, *v);
    }
    put_usize(b, c.methods.len());
    for (n, v, idx, is_ctor) in &c.methods {
        put_str(b, n);
        put_vis(b, *v);
        put_u16(b, *idx);
        put_bool(b, *is_ctor);
    }
}

/// Serialize a top-level script function to a `.matrixc` byte buffer. The
/// buffer begins with the format magic and a version word so a corrupt or
/// non-Spid3r file is rejected cleanly by `deserialize_vmfunc` rather than
/// parsed as if it were valid bytecode.
pub fn serialize_vmfunc(func: &VmFunc) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(FORMAT_MAGIC);
    b.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    put_vmfunc(&mut b, func);
    b
}

// ----------------------------------------------------------------- deserialize

/// Bounds-checked reader over a `.matrixc` byte buffer. Every read returns a
/// clean `Diagnostic` on truncation/out-of-range instead of indexing out of
/// bounds or panicking, so a corrupt or hand-edited file fails with
/// "invalid file format" rather than crashing the host.
struct Reader<'a> {
    b: &'a [u8],
    p: usize,
    /// Nesting depth of `get_chunk` (functions, class methods, embedded
    /// modules). A crafted file can nest without bound, which would recurse
    /// until the host's stack overflowed (an uncatchable abort); the limit is
    /// far above anything the compiler can emit (parser depth is 96).
    depth: usize,
}

/// Deepest function/module nesting accepted while deserializing a `.matrixc`.
const MAX_DESERIALIZE_DEPTH: usize = 128;

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader {
            b,
            p: 0,
            depth: 0,
        }
    }

    fn u8(&mut self) -> SpidResult<u8> {
        if self.p >= self.b.len() {
            return Err(err(Phase::Vm, "invalid file format: unexpected end of file"));
        }
        let v = self.b[self.p];
        self.p += 1;
        Ok(v)
    }

    fn u16(&mut self) -> SpidResult<u16> {
        if self.p + 2 > self.b.len() {
            return Err(err(Phase::Vm, "invalid file format: unexpected end of file"));
        }
        let v = u16::from_le_bytes([self.b[self.p], self.b[self.p + 1]]);
        self.p += 2;
        Ok(v)
    }

    fn usize(&mut self) -> SpidResult<usize> {
        if self.p + 8 > self.b.len() {
            return Err(err(Phase::Vm, "invalid file format: unexpected end of file"));
        }
        let mut a = [0u8; 8];
        a.copy_from_slice(&self.b[self.p..self.p + 8]);
        self.p += 8;
        Ok(u64::from_le_bytes(a) as usize)
    }

    fn bool(&mut self) -> SpidResult<bool> {
        Ok(self.u8()? != 0)
    }

    fn str(&mut self) -> SpidResult<String> {
        let n = self.usize()?;
        // checked_add: a crafted length near usize::MAX would otherwise wrap
        // `self.p + n` past the bounds check and panic the host on the slice.
        match n.checked_add(self.p) {
            Some(end) if end <= self.b.len() => {
                let s = String::from_utf8_lossy(&self.b[self.p..end]).into_owned();
                self.p = end;
                Ok(s)
            }
            _ => Err(err(Phase::Vm, "invalid file format: string length exceeds file")),
        }
    }

    fn opt_str(&mut self) -> SpidResult<Option<String>> {
        if self.u8()? == 1 {
            Ok(Some(self.str()?))
        } else {
            Ok(None)
        }
    }

    fn count(&mut self, min_bytes_per_item: usize) -> SpidResult<usize> {
        let n = self.usize()?;
        let rem = self.b.len().saturating_sub(self.p);
        let needed = n.saturating_mul(min_bytes_per_item.max(1));
        if needed > rem {
            return Err(err(
                Phase::Vm,
                "invalid file format: item count exceeds remaining file length",
            ));
        }
        Ok(n)
    }

    fn value(&mut self) -> SpidResult<Value> {
        let tag = self.u8()?;
        let mut a = [0u8; 8];
        match tag {
            0 => {
                if self.p + 8 > self.b.len() {
                    return Err(err(Phase::Vm, "invalid file format: unexpected end of file"));
                }
                a.copy_from_slice(&self.b[self.p..self.p + 8]);
                self.p += 8;
                Ok(Value::Int(u64::from_le_bytes(a) as i64))
            }
            1 => {
                if self.p + 8 > self.b.len() {
                    return Err(err(Phase::Vm, "invalid file format: unexpected end of file"));
                }
                a.copy_from_slice(&self.b[self.p..self.p + 8]);
                self.p += 8;
                Ok(Value::Float(f64::from_bits(u64::from_le_bytes(a))))
            }
            2 => Ok(Value::Str(self.str()?.into())),
            _ => Err(err(
                Phase::Vm,
                format!("invalid file format: corrupt constant tag {}", tag),
            )),
        }
    }

    fn vis(&mut self) -> SpidResult<Vis> {
        match self.u8()? {
            0 => Ok(Vis::Private),
            1 => Ok(Vis::Public),
            other => Err(err(
                Phase::Vm,
                format!("invalid file format: bad visibility byte {}", other),
            )),
        }
    }

    fn span(&mut self) -> SpidResult<Span> {
        let file = self.str()?;
        let line = self.usize()? as u32;
        let col = self.usize()? as u32;
        let len = self.usize()? as u32;
        Ok(Span::new(Arc::from(file), line, col, len))
    }
}

fn get_chunk(r: &mut Reader) -> SpidResult<Chunk> {
    if r.depth >= MAX_DESERIALIZE_DEPTH {
        return Err(err(
            Phase::Vm,
            format!(
                "invalid file format: function/module nesting exceeds the depth limit ({})",
                MAX_DESERIALIZE_DEPTH
            ),
        ));
    }
    r.depth += 1;
    let out = get_chunk_body(r);
    r.depth -= 1;
    out
}

fn get_chunk_body(r: &mut Reader) -> SpidResult<Chunk> {
    let code_len = r.usize()?;
    if code_len > r.b.len().saturating_sub(r.p) {
        return Err(err(Phase::Vm, "invalid file format: code length exceeds file"));
    }
    let mut code = vec![0u8; code_len];
    code.copy_from_slice(&r.b[r.p..r.p + code_len]);
    r.p += code_len;
    let n = r.count(9)?;
    let mut constants = Vec::with_capacity(n);
    for _ in 0..n {
        constants.push(r.value()?);
    }
    let n = r.count(1)?;
    let mut functions = Vec::with_capacity(n);
    for _ in 0..n {
        functions.push(std::sync::Arc::new(get_func_proto(r)?));
    }
    let n = r.count(1)?;
    let mut classes = Vec::with_capacity(n);
    for _ in 0..n {
        classes.push(get_class_proto(r)?);
    }
    let n = r.count(8)?;
    let mut inst_offsets = Vec::with_capacity(n);
    for _ in 0..n {
        inst_offsets.push(r.usize()?);
    }
    let n = r.count(32)?;
    let mut inst_spans = Vec::with_capacity(n);
    for _ in 0..n {
        inst_spans.push(r.span()?);
    }
    let pending_span = r.span()?;
    let n = r.count(1)?;
    let mut modules = Vec::with_capacity(n);
    for _ in 0..n {
        modules.push(get_embedded(r)?);
    }
    Ok(Chunk {
        code,
        constants,
        functions,
        classes,
        inst_offsets,
        inst_spans,
        pending_span,
        modules,
    })
}

fn get_func_proto(r: &mut Reader) -> SpidResult<VmFuncProto> {
    let name = r.opt_str()?;
    let arity = r.usize()?;
    let is_async = r.bool()?;
    let n = r.count(1)?;
    let mut upvalues = Vec::with_capacity(n);
    for _ in 0..n {
        upvalues.push(r.u8()?);
    }
    let own_upvalues = r.u8()?;
    let chunk = std::sync::Arc::new(get_chunk(r)?);
    Ok(VmFuncProto {
        name,
        arity,
        is_async,
        upvalues,
        own_upvalues,
        chunk,
    })
}

fn get_class_proto(r: &mut Reader) -> SpidResult<VmClassProto> {
    let name = r.str()?;
    let parent = r.opt_str()?;
    let n = r.count(9)?;
    let mut fields = Vec::with_capacity(n);
    for _ in 0..n {
        let fn_ = r.str()?;
        let vis = r.vis()?;
        fields.push((fn_, vis));
    }
    let n = r.count(12)?;
    let mut methods = Vec::with_capacity(n);
    for _ in 0..n {
        let mn = r.str()?;
        let vis = r.vis()?;
        let idx = r.u16()?;
        let is_ctor = r.bool()?;
        methods.push((mn, vis, idx, is_ctor));
    }
    Ok(VmClassProto {
        name,
        parent,
        fields,
        methods,
    })
}

fn get_embedded(r: &mut Reader) -> SpidResult<EmbeddedModule> {
    let key = r.str()?;
    let n = r.count(8)?;
    let mut public = Vec::with_capacity(n);
    for _ in 0..n {
        public.push(r.str()?);
    }
    let func = get_vmfunc(r)?;
    Ok(EmbeddedModule {
        key,
        public,
        func: Arc::new(func),
    })
}

fn get_vmfunc(r: &mut Reader) -> SpidResult<VmFunc> {
    let name = r.opt_str()?;
    let arity = r.usize()?;
    let is_async = r.bool()?;
    let n = r.count(9)?;
    let mut upvalues = Vec::with_capacity(n);
    for _ in 0..n {
        let v = r.value()?;
        upvalues.push(crate::value::new_untracked_ref(v));
    }
    let own_upvalues = r.u8()?;
    let chunk = std::sync::Arc::new(get_chunk(r)?);
    Ok(VmFunc {
        name,
        arity,
        is_async,
        chunk,
        upvalues,
        own_upvalues,
    })
}

/// Deserialize a `.matrixc` byte buffer back into a script `VmFunc`.
///
/// Rejects a file that is missing the magic number, has an unsupported version,
/// or is otherwise truncated/corrupt with a clean `Diagnostic` (never a panic).
pub fn deserialize_vmfunc(b: &[u8]) -> SpidResult<VmFunc> {
    if b.len() < 8 {
        return Err(err(
            Phase::Vm,
            "invalid file format: file too short to be a Spid3r bytecode file",
        ));
    }
    if &b[0..6] != FORMAT_MAGIC {
        return Err(err(
            Phase::Vm,
            "invalid file format: not a Spid3r bytecode file (missing magic number)",
        ));
    }
    let mut r = Reader::new(b);
    r.p = 6;
    let version = r.u16()?;
    if version != FORMAT_VERSION {
        return Err(err(
            Phase::Vm,
            format!(
                "unsupported bytecode version {} (expected {})",
                version, FORMAT_VERSION
            ),
        ));
    }
    let func = get_vmfunc(&mut r)?;
    // Trailing-byte rejection (audit A-8): a file whose declared structures do
    // not consume the whole buffer is malformed. Accepting it silently let an
    // attacker append data that a later reader might interpret differently.
    if r.p != b.len() {
        return Err(err(
            Phase::Vm,
            format!(
                "invalid file format: {} trailing byte(s) after the last structure",
                b.len() - r.p
            ),
        ));
    }
    Ok(func)
}

// ----------------------------------------------------------------- validate

/// Recursively validate a chunk (and every nested function / class-method chunk)
/// before it is executed by the VM. This is the safety net required by the
/// Spid3r spec: it rejects invalid opcodes, truncated instructions, out-of-range
/// constant/function/class references, and jump targets that do not land on an
/// instruction boundary — all with a clean `Diagnostic` rather than a panic or
/// undefined behaviour.
pub fn validate_chunk(chunk: &Chunk) -> SpidResult<()> {
    validate_chunk_inner(chunk, "<script>")
}

fn validate_chunk_inner(chunk: &Chunk, where_: &str) -> SpidResult<()> {
    let code = &chunk.code;
    let len = code.len();
    if len == 0 {
        return Err(err(
            Phase::Vm,
            format!("empty code section in {} (a chunk must end with a return)", where_),
        ));
    }
    // Instruction metadata must line up with the code: `span_at` maps an
    // offset through `inst_offsets` into `inst_spans`, and the deserializer
    // reads the two tables with independent counts. A crafted file could ship
    // more offsets than spans (host panic in `span_at`) or offsets past the
    // code (audit A-8).
    if chunk.inst_offsets.len() != chunk.inst_spans.len() {
        return Err(err(
            Phase::Vm,
            format!(
                "instruction tables disagree: {} offset(s) but {} span(s) in {}",
                chunk.inst_offsets.len(),
                chunk.inst_spans.len(),
                where_
            ),
        ));
    }
    if !chunk.inst_offsets.windows(2).all(|w| w[0] < w[1]) {
        return Err(err(
            Phase::Vm,
            format!("instruction offsets are not strictly increasing in {}", where_),
        ));
    }
    if let Some(&last) = chunk.inst_offsets.last() {
        if last >= len {
            return Err(err(
                Phase::Vm,
                format!(
                    "instruction offset {} is outside the code section ({} bytes) in {}",
                    last, len, where_
                ),
            ));
        }
    }

    let mut ip = 0usize;
    // Offsets at which a real instruction begins; jump targets must land here.
    let mut starts: Vec<usize> = Vec::new();
    // (jump_source_offset, target_offset) pairs to verify after the full scan.
    let mut jumps: Vec<(usize, i64)> = Vec::new();

    while ip < len {
        starts.push(ip);
        let byte = code[ip];
        let op = match OpCode::from_u8(byte) {
            Some(o) => o,
            None => {
                return Err(err(
                    Phase::Vm,
                    format!(
                        "invalid opcode 0x{:02x} at offset {} in {}",
                        byte, ip, where_
                    ),
                ))
            }
        };
        let oplen = operand_len(op);
        if ip + 1 + oplen > len {
            return Err(err(
                Phase::Vm,
                format!(
                    "truncated instruction {:?} at offset {} in {}",
                    op, ip, where_
                ),
            ));
        }
        match op {
            OpCode::Closure => {
                let idx = u16::from_le_bytes([code[ip + 1], code[ip + 2]]) as usize;
                check_pool("function", chunk.functions.len(), idx, where_)?;
            }
            OpCode::Class => {
                let idx = u16::from_le_bytes([code[ip + 1], code[ip + 2]]) as usize;
                check_pool("class", chunk.classes.len(), idx, where_)?;
            }
            OpCode::Constant
            | OpCode::GetGlobal
            | OpCode::SetGlobal
            | OpCode::DefineGlobal
            | OpCode::FieldGet
            | OpCode::FieldSet
            | OpCode::Import => {
                let idx = u16::from_le_bytes([code[ip + 1], code[ip + 2]]) as usize;
                check_pool("constant", chunk.constants.len(), idx, where_)?;
            }
            OpCode::ImportAs => {
                let p_idx = u16::from_le_bytes([code[ip + 1], code[ip + 2]]) as usize;
                let a_idx = u16::from_le_bytes([code[ip + 3], code[ip + 4]]) as usize;
                check_pool("constant", chunk.constants.len(), p_idx, where_)?;
                check_pool("constant", chunk.constants.len(), a_idx, where_)?;
                if !matches!(chunk.constants.get(p_idx), Some(Value::Str(_))) {
                    return Err(err(
                        Phase::Vm,
                        format!(
                            "ImportAs path operand at index {} is not a string in {}",
                            p_idx, where_
                        ),
                    ));
                }
                if !matches!(chunk.constants.get(a_idx), Some(Value::Str(_))) {
                    return Err(err(
                        Phase::Vm,
                        format!(
                            "ImportAs alias operand at index {} is not a string in {}",
                            a_idx, where_
                        ),
                    ));
                }
            }
            OpCode::Construct | OpCode::Invoke => {
                let idx = u16::from_le_bytes([code[ip + 1], code[ip + 2]]) as usize;
                check_pool("constant", chunk.constants.len(), idx, where_)?;
                if !matches!(chunk.constants.get(idx), Some(Value::Str(_))) {
                    return Err(err(
                        Phase::Vm,
                        format!(
                            "{:?} name operand at index {} is not a string in {}",
                            op, idx, where_
                        ),
                    ));
                }
            }
            OpCode::Jump | OpCode::JumpIfFalse | OpCode::JumpIfTrue | OpCode::Loop => {
                let off = i16::from_le_bytes([code[ip + 1], code[ip + 2]]);
                let target = ip as i64 + 3 + off as i64;
                jumps.push((ip, target));
            }
            OpCode::Try => {
                let catch_off = i16::from_le_bytes([code[ip + 3], code[ip + 4]]);
                let finally_off = i16::from_le_bytes([code[ip + 5], code[ip + 6]]);
                let operand_end = (ip + 7) as i64;
                jumps.push((ip, operand_end + catch_off as i64));
                jumps.push((ip, operand_end + finally_off as i64));
            }
            _ => {}
        }
        ip += 1 + oplen;
    }

    // Terminator rule (audit A-8): the VM advances `ip` past the last
    // instruction when control falls through, and `code[ip]` would then be out
    // of range. Every chunk the compiler emits ends with `Return`, so require
    // an instruction that cannot fall through. A `Jump` here is safe: its
    // target was just validated to be inside the code and on an instruction
    // boundary (a forward jump from the last instruction cannot be in range).
    if let Some(&last_start) = starts.last() {
        let op = OpCode::from_u8(code[last_start]).unwrap_or(OpCode::Return);
        if !matches!(op, OpCode::Return | OpCode::Throw | OpCode::Jump) {
            return Err(err(
                Phase::Vm,
                format!(
                    "chunk ends with a fall-through instruction {:?} at offset {} in {} \
                     (a chunk must end with return, throw, or jump)",
                    op, last_start, where_
                ),
            ));
        }
    }

    for (src, target) in jumps {
        // `starts` is sorted: binary search instead of `contains`, which made
        // validation quadratic (a 10 MB file with many jumps took ~10^13
        // comparisons — a CPU DoS at every `exec` entry; audit A-8).
        let in_range = target >= 0 && (target as usize) < len;
        let on_boundary = in_range && starts.binary_search(&(target as usize)).is_ok();
        if !on_boundary {
            return Err(err(
                Phase::Vm,
                format!("invalid jump target {} from offset {} in {}", target, src, where_),
            ));
        }
    }

    // Constants must be encodable by the `.matrixc` codec. Bool/Null have
    // dedicated opcodes and must never appear in the pool, so a pool entry of
    // another kind is a compiler bug — report it instead of panicking later in
    // `put_value`.
    for (i, c) in chunk.constants.iter().enumerate() {
        if !matches!(c, Value::Int(_) | Value::Float(_) | Value::Str(_)) {
            return Err(err(
                Phase::Vm,
                format!(
                    "constant #{} has a value that cannot be encoded ({}); \
                     bool/null use dedicated opcodes",
                    i,
                    crate::value::type_name(c)
                ),
            ));
        }
    }

    for f in &chunk.functions {
        validate_chunk_inner(&f.chunk, "<fn>")?;
    }
    for c in &chunk.classes {
        for (_m, _v, fidx, _c) in &c.methods {
            let idx = *fidx as usize;
            if idx >= chunk.functions.len() {
                return Err(err(
                    Phase::Vm,
                    format!("class method references invalid function index {} in {}", idx, where_),
                ));
            }
            validate_chunk_inner(&chunk.functions[idx].chunk, "<method>")?;
        }
    }
    for m in &chunk.modules {
        validate_chunk_inner(&m.func.chunk, &m.key)?;
    }
    Ok(())
}

/// How many operand bytes follow a given opcode.
pub(crate) fn operand_len(op: OpCode) -> usize {
    match op {
        OpCode::GetLocal
        | OpCode::SetLocal
        | OpCode::GetUpvalue
        | OpCode::SetUpvalue
        | OpCode::Call => 1,
        OpCode::Constant
        | OpCode::GetGlobal
        | OpCode::SetGlobal
        | OpCode::DefineGlobal
        | OpCode::Closure
        | OpCode::FieldGet
        | OpCode::FieldSet
        | OpCode::Import
        | OpCode::Class => 2,
        OpCode::Construct | OpCode::Invoke => 3,
        OpCode::ImportAs => 4,
        OpCode::CaptureSync => 2,
        OpCode::Uncapture => 1,
        OpCode::Jump | OpCode::JumpIfFalse | OpCode::JumpIfTrue | OpCode::Loop => 2,
        OpCode::Try => 6,
        _ => 0,
    }
}

fn check_pool(kind: &str, pool_len: usize, idx: usize, where_: &str) -> SpidResult<()> {
    if idx >= pool_len {
        return Err(err(
            Phase::Vm,
            format!(
                "{} index {} out of range (pool size {}) in {}",
                kind, idx, pool_len, where_
            ),
        ));
    }
    Ok(())
}

// --------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_u8_rejects_garbage_without_ub() {
        // Any byte outside 0..=54 must decode to None (no transmute / UB).
        for b in 55u16..=255 {
            assert!(OpCode::from_u8(b as u8).is_none(), "byte {} decoded", b);
        }
        // A handful of real opcodes round-trip.
        assert_eq!(OpCode::from_u8(0), Some(OpCode::Constant));
        assert_eq!(OpCode::from_u8(44), Some(OpCode::Import));
        assert_eq!(OpCode::from_u8(49), Some(OpCode::EnterFinally));
        assert_eq!(OpCode::from_u8(50), Some(OpCode::Dup));
        assert_eq!(OpCode::from_u8(51), Some(OpCode::Dup2));
        assert_eq!(OpCode::from_u8(52), Some(OpCode::CaptureSync));
        assert_eq!(OpCode::from_u8(53), Some(OpCode::Uncapture));
        assert_eq!(OpCode::from_u8(54), Some(OpCode::ImportAs));
    }

    #[test]
    fn validate_accepts_well_formed_chunk() {
        let mut c = Chunk::new();
        c.emit(OpCode::Null);
        c.emit(OpCode::Return);
        assert!(validate_chunk(&c).is_ok());
    }

    #[test]
    fn validate_rejects_invalid_opcode() {
        let mut c = Chunk::new();
        c.code.push(0xFF); // not a valid opcode
        assert!(validate_chunk(&c).is_err());
    }

    #[test]
    fn validate_rejects_truncated_instruction() {
        let mut c = Chunk::new();
        // OpConstant (opcode 0) needs a 2-byte operand; provide only 1 byte.
        c.code.push(0);
        c.code.push(0x12);
        assert!(validate_chunk(&c).is_err());
    }

    #[test]
    fn validate_rejects_out_of_range_constant() {
        let mut c = Chunk::new();
        // GetGlobal (8) with index 5, but the constant pool is empty.
        c.emit_u16(OpCode::GetGlobal, 5);
        assert!(validate_chunk(&c).is_err());
    }

    #[test]
    fn validate_rejects_off_boundary_jump() {
        let mut c = Chunk::new();
        // Jump (27) with a large offset that lands past the end of the code.
        c.emit_i16(OpCode::Jump, 100);
        assert!(validate_chunk(&c).is_err());
    }

    #[test]
    fn magic_and_version_are_correct() {
        assert_eq!(FORMAT_MAGIC, b"SPID3R");
        // A too-short buffer and a wrong magic are rejected by the deserializer.
        assert!(deserialize_vmfunc(b"short").is_err());
        assert!(deserialize_vmfunc(b"NOTSPID3R\x01\x00").is_err());
    }
}
