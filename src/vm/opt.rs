//! Bytecode optimizer: constant folding, peephole, and dead-code removal.
//!
//! Runs once after compilation (before serialization), on every chunk
//! reachable from the script: nested function prototypes, class methods, and
//! embedded module functions. Every transformation preserves observable
//! behaviour by construction:
//!
//! - **Constant folding** evaluates `Constant a; Constant b; <binop>` through
//!   `vm_eval_binary` — the exact helper the interpreter loop uses — so results
//!   (including numeric promotion and string concatenation) are identical. If
//!   the operation would raise at run time (e.g. `1 / 0`, `"a" - 1`), it is
//!   **not** folded, so the error still fires with the same message and span.
//! - A fold is skipped when any jump or handler target points into the middle
//!   of the folded sequence (a target must keep landing on a real
//!   instruction).
//! - **Peephole**: `Jump +0` and `<push constant>; Pop` pairs are removed;
//!   neither can have side effects.
//! - **Dead code**: unreachable instructions (after `Return`/`Jump`/`Throw`,
//!   up to the next label) are removed. Skipped entirely for chunks that
//!   contain `Try`, because handler regions are entered by non-local means.
//!
//! Jump offsets (`Jump`/`JumpIfFalse`/`JumpIfTrue`/`Loop`, relative to the
//! instruction's end) and `Try`'s two handler offsets are recomputed through an
//! old→new offset map. If any patched offset would not fit in `i16`, the whole
//! chunk is left untouched (the transformation is all-or-nothing per chunk).
//!
//! `inst_offsets`/`inst_spans` are rebuilt for the surviving instructions, so
//! error attribution and the debugger keep pointing at real source lines.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::ast::{BinOp, UnOp};
use crate::diagnostics::Span;
use crate::value::{Value, VmFunc};

use super::bytecode::{operand_len, Chunk, OpCode};
use super::{vm_eval_binary, vm_eval_unary};

/// What the optimizer did, for `--stats` / tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct OptStats {
    pub chunks: usize,
    pub folded: usize,
    pub removed: usize,
}

impl OptStats {
    pub fn changed(&self) -> bool {
        self.folded > 0 || self.removed > 0
    }
}

/// Optimize every chunk reachable from a compiled script function.
pub fn optimize_script(script: &mut VmFunc) -> OptStats {
    let mut stats = OptStats::default();
    optimize_chunk(Arc::make_mut(&mut script.chunk), &mut stats);
    stats
}

fn optimize_chunk(chunk: &mut Chunk, stats: &mut OptStats) {
    stats.chunks += 1;
    // Depth-first into nested prototypes, class methods, and embedded modules first.
    for f in chunk.functions.iter_mut() {
        let proto = Arc::make_mut(f);
        optimize_chunk(Arc::make_mut(&mut proto.chunk), stats);
    }
    for m in chunk.modules.iter_mut() {
        let func = Arc::make_mut(&mut m.func);
        optimize_chunk(Arc::make_mut(&mut func.chunk), stats);
    }
    optimize_body(chunk, stats);
}

/// One decoded instruction.
#[derive(Clone)]
struct Ins {
    op: OpCode,
    /// Operand bytes (excluding the opcode).
    ops: Vec<u8>,
    span: Span,
    /// Byte offset in the original code (jump targets are expressed in it).
    old: usize,
}

impl Ins {
    fn len(&self) -> usize {
        1 + self.ops.len()
    }

    fn u16_at(&self, i: usize) -> u16 {
        u16::from_le_bytes([self.ops[i], self.ops[i + 1]])
    }

    fn i16_at(&self, i: usize) -> i16 {
        i16::from_le_bytes([self.ops[i], self.ops[i + 1]])
    }

    /// Absolute target of a relative jump (offsets are relative to the end of
    /// the instruction), in ORIGINAL byte offsets.
    fn jump_target(&self) -> Option<usize> {
        match self.op {
            OpCode::Jump | OpCode::JumpIfFalse | OpCode::JumpIfTrue | OpCode::Loop => {
                Some((self.old as i64 + 3 + self.i16_at(0) as i64) as usize)
            }
            _ => None,
        }
    }

    /// Absolute handler targets of `Try`, in ORIGINAL byte offsets.
    fn try_targets(&self) -> Vec<usize> {
        if self.op != OpCode::Try {
            return Vec::new();
        }
        let catch = (self.old as i64 + 7 + self.i16_at(2) as i64) as usize;
        let finally = (self.old as i64 + 7 + self.i16_at(4) as i64) as usize;
        vec![catch, finally]
    }
}

fn decode(chunk: &Chunk) -> Option<Vec<Ins>> {
    let mut out = Vec::new();
    let mut ip = 0usize;
    while ip < chunk.code.len() {
        let op = OpCode::from_u8(chunk.code[ip])?;
        let n = operand_len(op);
        if ip + 1 + n > chunk.code.len() {
            return None;
        }
        out.push(Ins {
            op,
            ops: chunk.code[ip + 1..ip + 1 + n].to_vec(),
            span: chunk.span_at(ip),
            old: ip,
        });
        ip += 1 + n;
    }
    Some(out)
}

/// The binary opcode → AST operator mapping, for folding.
fn binop_of(op: OpCode) -> Option<BinOp> {
    Some(match op {
        OpCode::Add => BinOp::Add,
        OpCode::Sub => BinOp::Sub,
        OpCode::Mul => BinOp::Mul,
        OpCode::Div => BinOp::Div,
        OpCode::Mod => BinOp::Mod,
        OpCode::Equal => BinOp::Eq,
        OpCode::NotEqual => BinOp::Ne,
        OpCode::Less => BinOp::Lt,
        OpCode::LessEq => BinOp::Le,
        OpCode::Greater => BinOp::Gt,
        OpCode::GreaterEq => BinOp::Ge,
        _ => return None,
    })
}

fn unop_of(op: OpCode) -> Option<UnOp> {
    Some(match op {
        OpCode::Neg => UnOp::Neg,
        OpCode::Not => UnOp::Not,
        _ => return None,
    })
}

fn optimize_body(chunk: &mut Chunk, stats: &mut OptStats) {
    let Some(mut ins) = decode(chunk) else {
        return; // malformed: leave the chunk alone (validate_chunk reports it)
    };

    // Every offset a jump or handler can land on: folds may not cut through
    // them.
    let mut targets: HashSet<usize> = HashSet::new();
    let mut has_try = false;
    for i in &ins {
        if let Some(t) = i.jump_target() {
            targets.insert(t);
        }
        for t in i.try_targets() {
            targets.insert(t);
        }
        has_try |= i.op == OpCode::Try;
    }

    // New constants produced by folding, appended to the pool at encode time.
    let mut extra_consts: Vec<Value> = Vec::new();
    let mut folded = 0usize;
    let mut removed = 0usize;

    // Fold + peephole, repeated until stable (folding exposes more folding).
    let mut changed = true;
    while changed {
        changed = false;
        let mut i = 0usize;
        while i < ins.len() {
            // [Constant a][Constant b][binop] -> [Constant result]
            if i + 2 < ins.len()
                && ins[i].op == OpCode::Constant
                && ins[i + 1].op == OpCode::Constant
            {
                if let Some(op) = binop_of(ins[i + 2].op) {
                    let cut = [ins[i + 1].old, ins[i + 2].old];
                    if cut.iter().any(|o| targets.contains(o)) {
                        i += 1;
                        continue;
                    }
                    let a = const_at(chunk, &extra_consts, &ins[i]);
                    let b = const_at(chunk, &extra_consts, &ins[i + 1]);
                    if let (Some(a), Some(b)) = (a, b) {
                        // Only fold when the runtime helper succeeds: an error
                        // here must still surface at run time.
                        if let Ok(v) = vm_eval_binary(op, a, b) {
                            if is_foldable(&v) {
                                let span = ins[i + 2].span.clone();
                                let old = ins[i].old;
                                ins[i] = folded_push(chunk, &mut extra_consts, v, span, old);
                                ins.drain(i + 1..i + 3);
                                folded += 1;
                                changed = true;
                                continue;
                            }
                        }
                    }
                }
            }
            // [Constant a][Neg|Not] -> [Constant result]
            if i + 1 < ins.len() && ins[i].op == OpCode::Constant {
                if let Some(op) = unop_of(ins[i + 1].op) {
                    if !targets.contains(&ins[i + 1].old) {
                        if let Some(a) = const_at(chunk, &extra_consts, &ins[i]) {
                            if let Ok(v) = vm_eval_unary(op, a) {
                                if is_foldable(&v) {
                                    let span = ins[i + 1].span.clone();
                                    let old = ins[i].old;
                                    ins[i] = folded_push(chunk, &mut extra_consts, v, span, old);
                                    ins.remove(i + 1);
                                    folded += 1;
                                    changed = true;
                                    continue;
                                }
                            }
                        }
                    }
                }
            }
            // [Jump +0] -> nothing (a no-op jump).
            if ins[i].op == OpCode::Jump && ins[i].jump_target() == Some(ins[i].old + 3) {
                ins.remove(i);
                removed += 1;
                changed = true;
                continue;
            }
            // Branch folding: a conditional jump on a statically known truth
            // value. `JumpIfFalse`/`JumpIfTrue` pop their operand, so removing
            // a jump must also remove the value that was pushed for it, and
            // replacing one with an unconditional `Jump` must keep the stack
            // balanced (hence `[<bool>][jump]` pairs, never one alone).
            if i + 1 < ins.len() {
                let cond = match ins[i].op {
                    OpCode::True => Some(true),
                    OpCode::False => Some(false),
                    _ => None,
                };
                if let Some(cond) = cond {
                    let jump_op = ins[i + 1].op;
                    let is_cond_jump = matches!(jump_op, OpCode::JumpIfFalse | OpCode::JumpIfTrue);
                    // A label may not point into the middle of the pair.
                    if is_cond_jump && !targets.contains(&ins[i + 1].old) {
                        let takes_branch = match jump_op {
                            OpCode::JumpIfFalse => !cond,
                            OpCode::JumpIfTrue => cond,
                            _ => false,
                        };
                        if takes_branch {
                            // Keep the jump, drop the constant: the jump no
                            // longer pops anything.
                            let target = ins[i + 1].jump_target();
                            if let Some(t) = target {
                                let delta = t as i64 - (ins[i].old as i64 + 3);
                                if let Ok(d) = i16::try_from(delta) {
                                    ins[i] = Ins {
                                        op: OpCode::Jump,
                                        ops: d.to_le_bytes().to_vec(),
                                        span: ins[i + 1].span.clone(),
                                        old: ins[i].old,
                                    };
                                    ins.remove(i + 1);
                                    folded += 1;
                                    changed = true;
                                    continue;
                                }
                            }
                        } else {
                            // Condition is already the value we want: drop
                            // both the constant and the jump.
                            ins.remove(i + 1);
                            ins.remove(i);
                            removed += 2;
                            changed = true;
                            continue;
                        }
                    }
                }
                // [Not][JumpIfFalse] -> [JumpIfTrue] (and vice versa): the
                // `Not` only exists to invert the branch.
                if ins[i].op == OpCode::Not
                    && matches!(ins[i + 1].op, OpCode::JumpIfFalse | OpCode::JumpIfTrue)
                    && !targets.contains(&ins[i + 1].old)
                {
                    ins[i + 1].op = if ins[i + 1].op == OpCode::JumpIfFalse {
                        OpCode::JumpIfTrue
                    } else {
                        OpCode::JumpIfFalse
                    };
                    ins.remove(i);
                    removed += 1;
                    changed = true;
                    continue;
                }
                // [Not][Not] -> [Truthy]: `not not x` is exactly "is x truthy".
                if ins[i].op == OpCode::Not
                    && ins[i + 1].op == OpCode::Not
                    && !targets.contains(&ins[i + 1].old)
                {
                    ins[i] = Ins {
                        op: OpCode::Truthy,
                        ops: Vec::new(),
                        span: ins[i].span.clone(),
                        old: ins[i].old,
                    };
                    ins.remove(i + 1);
                    folded += 1;
                    changed = true;
                    continue;
                }
                // [True|False|Null][Not] -> [False|True|True]
                if ins[i + 1].op == OpCode::Not
                    && !targets.contains(&ins[i + 1].old)
                    && matches!(ins[i].op, OpCode::True | OpCode::False | OpCode::Null)
                {
                    let v = ins[i].op == OpCode::True;
                    ins[i] = Ins {
                        op: if v { OpCode::False } else { OpCode::True },
                        ops: Vec::new(),
                        span: ins[i].span.clone(),
                        old: ins[i].old,
                    };
                    ins.remove(i + 1);
                    folded += 1;
                    changed = true;
                    continue;
                }
            }
            // [<push constant>][Pop] -> nothing.
            if i + 1 < ins.len()
                && is_const_push(&ins[i].op)
                && ins[i + 1].op == OpCode::Pop
                && !targets.contains(&ins[i + 1].old)
            {
                ins.remove(i + 1);
                ins.remove(i);
                removed += 2;
                changed = true;
                continue;
            }
            i += 1;
        }
    }

    // Dead code: instructions unreachable from the entry point, any jump
    // target, or a handler target. Skipped when the chunk has `Try`, whose
    // regions are entered non-locally.
    if !has_try {
        let index_of: HashMap<usize, usize> =
            ins.iter().enumerate().map(|(k, i)| (i.old, k)).collect();
        // A fold can remove an instruction that a surviving jump targets (e.g.
        // `[False][JumpIfFalse -> A]` becomes `[Jump -> A]`, then the
        // `[True][JumpIfFalse]` pair at A itself folds away). Such a dangling
        // target must resolve to the next surviving instruction — the same
        // forward-fill the encode step applies through `remap` — otherwise the
        // dead-code pass would mark the reachable code after it unreachable
        // and delete live branches.
        let mut starts: Vec<usize> = index_of.keys().copied().collect();
        starts.sort_unstable();
        let resolve = |t: usize| -> Option<usize> {
            if let Some(&k) = index_of.get(&t) {
                return Some(k);
            }
            let pos = starts.partition_point(|&o| o <= t);
            if pos < starts.len() {
                Some(index_of[&starts[pos]])
            } else {
                None // past the end: nothing left to run
            }
        };
        let mut reachable = vec![false; ins.len()];
        let mut stack: Vec<usize> = vec![0];
        for t in &targets {
            if let Some(k) = resolve(*t) {
                stack.push(k);
            }
        }
        while let Some(k) = stack.pop() {
            if k >= ins.len() || reachable[k] {
                continue;
            }
            reachable[k] = true;
            match ins[k].op {
                OpCode::Return | OpCode::Throw | OpCode::Jump => {}
                _ => stack.push(k + 1),
            }
            if let Some(t) = ins[k].jump_target() {
                if let Some(tk) = resolve(t) {
                    stack.push(tk);
                }
            }
        }
        if reachable.iter().any(|r| !r) {
            let before = ins.len();
            let mut keep = reachable.iter();
            ins.retain(|_| *keep.next().unwrap());
            removed += before - ins.len();
        }
    }

    if folded == 0 && removed == 0 {
        return;
    }

    // Assign new offsets and build the old→new map (removed instructions map
    // to the next surviving instruction).
    let mut new_off = 0usize;
    let mut remap: HashMap<usize, usize> = HashMap::new();
    let mut layout: Vec<(usize, usize)> = Vec::with_capacity(ins.len()); // (old, new)
    for i in &ins {
        layout.push((i.old, new_off));
        new_off += i.len();
    }
    for (old, new) in &layout {
        remap.insert(*old, *new);
    }
    // Removed offsets: forward-fill from the next surviving instruction.
    {
        let mut next_new = new_off; // end of code: nothing left to run
        let mut sorted: Vec<usize> = layout.iter().map(|(o, _)| *o).collect();
        sorted.sort_unstable();
        // Walk the ORIGINAL offsets backwards over the original code so every
        // offset (including removed ones) resolves to the next survivor.
        let mut all_old: Vec<usize> = Vec::new();
        let mut p = 0usize;
        while p < chunk.code.len() {
            all_old.push(p);
            let op = match OpCode::from_u8(chunk.code[p]) {
                Some(o) => o,
                None => break,
            };
            p += 1 + operand_len(op);
        }
        for o in all_old.iter().rev() {
            if let Some(n) = remap.get(o) {
                next_new = *n;
            } else {
                remap.insert(*o, next_new);
            }
        }
    }

    // Encode: patch relative offsets through the map. Any overflow aborts the
    // whole chunk (nothing has been mutated yet).
    let mut code: Vec<u8> = Vec::with_capacity(chunk.code.len());
    let mut inst_offsets: Vec<usize> = Vec::with_capacity(ins.len());
    let mut inst_spans: Vec<Span> = Vec::with_capacity(ins.len());
    let mut ok = true;
    for i in &ins {
        let here = code.len();
        inst_offsets.push(here);
        inst_spans.push(i.span.clone());
        code.push(i.op as u8);
        match i.op {
            OpCode::Jump | OpCode::JumpIfFalse | OpCode::JumpIfTrue | OpCode::Loop => {
                let Some(target) = i.jump_target() else {
                    ok = false;
                    break;
                };
                let Some(&new_target) = remap.get(&target) else {
                    ok = false;
                    break;
                };
                let delta = new_target as i64 - (here as i64 + 3);
                match i16::try_from(delta) {
                    Ok(d) => code.extend_from_slice(&d.to_le_bytes()),
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            OpCode::Try => {
                let (catch, finally) = (i.try_targets()[0], i.try_targets()[1]);
                let (Some(&nc), Some(&nf_)) = (remap.get(&catch), remap.get(&finally)) else {
                    ok = false;
                    break;
                };
                let dc = nc as i64 - (here as i64 + 7);
                let df = nf_ as i64 - (here as i64 + 7);
                match (i16::try_from(dc), i16::try_from(df)) {
                    (Ok(dc), Ok(df)) => {
                        code.extend_from_slice(&i.ops);
                        let base = code.len() - 4;
                        code[base..base + 2].copy_from_slice(&dc.to_le_bytes());
                        code[base + 2..base + 4].copy_from_slice(&df.to_le_bytes());
                    }
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            _ => code.extend_from_slice(&i.ops),
        }
    }
    if !ok {
        return;
    }

    // Commit.
    chunk.constants.extend(extra_consts);
    chunk.code = code;
    chunk.inst_offsets = inst_offsets;
    chunk.inst_spans = inst_spans;
    stats.folded += folded;
    stats.removed += removed;
}

/// Value of a `Constant` instruction (from the pool or the pending new ones).
fn const_at(chunk: &Chunk, extra: &[Value], ins: &Ins) -> Option<Value> {
    if ins.op != OpCode::Constant || ins.ops.len() < 2 {
        return None;
    }
    let idx = ins.u16_at(0) as usize;
    if idx < chunk.constants.len() {
        Some(chunk.constants[idx].clone())
    } else {
        extra.get(idx - chunk.constants.len()).cloned()
    }
}

/// Only plain scalar constants are folded back into the pool (the `.matrixc`
/// codec can serialize exactly Int/Float/Str; Bool and Null have dedicated
/// opcodes, see `folded_push`).
fn is_foldable(v: &Value) -> bool {
    matches!(
        v,
        Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Bool(_) | Value::Null
    )
}

/// The instruction that pushes a folded value: `True`/`False`/`Null` opcodes
/// for the dedicated values, a `Constant` otherwise. Booleans and null are
/// deliberately NOT put in the constant pool: the bytecode codec only encodes
/// Int/Float/Str.
fn folded_push(chunk: &Chunk, extra: &mut Vec<Value>, v: Value, span: Span, old: usize) -> Ins {
    match v {
        Value::Bool(true) => Ins {
            op: OpCode::True,
            ops: Vec::new(),
            span,
            old,
        },
        Value::Bool(false) => Ins {
            op: OpCode::False,
            ops: Vec::new(),
            span,
            old,
        },
        Value::Null => Ins {
            op: OpCode::Null,
            ops: Vec::new(),
            span,
            old,
        },
        other => {
            let idx = new_const_index(chunk, extra, other);
            Ins {
                op: OpCode::Constant,
                ops: (idx as u16).to_le_bytes().to_vec(),
                span,
                old,
            }
        }
    }
}

fn new_const_index(chunk: &Chunk, extra: &mut Vec<Value>, v: Value) -> usize {
    let base = chunk.constants.len();
    for (k, e) in extra.iter().enumerate() {
        if same_const(e, &v) {
            return base + k;
        }
    }
    extra.push(v);
    base + extra.len() - 1
}

fn same_const(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x.to_bits() == y.to_bits(),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

/// Opcodes that push a value without side effects (for `<push>; Pop` removal).
fn is_const_push(op: &OpCode) -> bool {
    matches!(
        op,
        OpCode::Constant | OpCode::Null | OpCode::True | OpCode::False | OpCode::None
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser;
    use crate::semantic;

    fn compile(src: &str) -> VmFunc {
        let program = parser::parse_file(src, Arc::from("t.matrix")).expect("parse");
        semantic::check_file(&program, true, "t.matrix").expect("check");
        super::super::compile_program_unoptimized(&program, "t.matrix", &[])
            .expect("compile")
            .0
    }

    /// Total bytecode size across the script chunk and every nested chunk.
    fn total_code_len(chunk: &Chunk) -> usize {
        chunk.code.len()
            + chunk
                .functions
                .iter()
                .map(|f| total_code_len(&f.chunk))
                .sum::<usize>()
    }

    #[test]
    fn folds_arithmetic_constants() {
        let mut f =
            compile("fn main() {\n    let x = 1 + 2 * 3;\n    println(x);\n    return 0;\n}\n");
        let before = total_code_len(&f.chunk);
        let stats = optimize_script(&mut f);
        assert!(stats.folded >= 2, "expected folds, got {:?}", stats);
        assert!(
            total_code_len(&f.chunk) < before,
            "code should shrink: {} -> {}",
            before,
            total_code_len(&f.chunk)
        );
    }

    #[test]
    fn does_not_fold_operations_that_would_raise() {
        // `1 / 0` must still raise at run time.
        let mut f = compile("fn main() {\n    let x = 1 / 0;\n    println(x);\n    return 0;\n}\n");
        let stats = optimize_script(&mut f);
        assert_eq!(stats.folded, 0, "division by zero must not be folded");
    }

    #[test]
    fn folds_string_concat_and_unary() {
        let mut f = compile(
            "fn main() {\n    println(\"a\" + \"b\");\n    println(-5);\n    return 0;\n}\n",
        );
        let stats = optimize_script(&mut f);
        assert!(stats.folded >= 2, "expected folds, got {:?}", stats);
    }

    /// Count instructions of one opcode across the script and nested chunks.
    fn count_op(chunk: &Chunk, want: OpCode) -> usize {
        let mut n = chunk
            .code
            .iter()
            .filter(|b| OpCode::from_u8(**b) == Some(want))
            .count();
        for f in &chunk.functions {
            n += count_op(&f.chunk, want);
        }
        n
    }

    #[test]
    fn folds_static_branches() {
        // `1 < 2` folds to True, the branch then folds away, and the dead arm
        // is removed: the whole `if` collapses to the taken arm.
        let src = "fn main() {\n    if 1 < 2 {\n        println(\"a\");\n    } else {\n        println(\"b\");\n    }\n    return 0;\n}\n";
        let mut f = compile(src);
        let before = total_code_len(&f.chunk);
        let jumps_before = count_op(&f.chunk, OpCode::JumpIfFalse);
        let stats = optimize_script(&mut f);
        let after = total_code_len(&f.chunk);
        assert!(
            stats.folded >= 1 && stats.removed >= 3,
            "expected folding + dead-arm removal, got {:?}",
            stats
        );
        assert!(
            after < before,
            "code should shrink: {} -> {}",
            before,
            after
        );
        assert_eq!(
            count_op(&f.chunk, OpCode::JumpIfFalse),
            0,
            "the static branch should be gone"
        );
        assert!(jumps_before >= 1, "precondition: source had a branch");
    }

    #[test]
    fn dynamic_conditions_are_not_folded() {
        let src = "fn main() {\n    let x = 3;\n    if x > 1 {\n        println(\"a\");\n    } else {\n        println(\"b\");\n    }\n    return 0;\n}\n";
        let mut f = compile(src);
        let before = count_op(&f.chunk, OpCode::JumpIfFalse);
        let stats = optimize_script(&mut f);
        // No static truth value here: the branch must survive untouched.
        assert_eq!(stats.folded, 0, "dynamic condition folded: {:?}", stats);
        assert_eq!(count_op(&f.chunk, OpCode::JumpIfFalse), before);
    }

    #[test]
    fn double_negation_becomes_truthy() {
        let src =
            "fn main() {\n    println(not not 0);\n    println(not not 1);\n    return 0;\n}\n";
        let mut f = compile(src);
        let stats = optimize_script(&mut f);
        assert!(
            stats.folded >= 2,
            "expected not-not folding, got {:?}",
            stats
        );
        let mut vm = super::super::Vm::new(Arc::from("t.matrix"), Vec::new());
        vm.run_compiled(Arc::new(f)).expect("run");
    }

    #[test]
    fn optimized_and_unoptimized_agree_on_output() {
        // Compiled bytecode must behave identically with and without the
        // optimizer (checked end-to-end by the differential suite too).
        let src = "fn main() {\n    let a = 2 + 3 * 4;\n    let b = \"x\" + \"y\";\n    println(a);\n    println(b);\n    println(10 / 4);\n    return 0;\n}\n";
        let mut opt = compile(src);
        optimize_script(&mut opt);
        let plain = compile(src);
        // Same constant pool size is not required; same behaviour is.
        let run = |f: VmFunc| {
            let mut vm = super::super::Vm::new(Arc::from("t.matrix"), Vec::new());
            vm.run_compiled(Arc::new(f)).expect("run")
        };
        assert_eq!(run(opt), run(plain));
    }
}
