//! Pre-decoded fixed-width instruction stream and general superinstructions
//! for the Spid3r VM (`run_loop`).
//!
//! Bytecode chunks remain compact byte streams on disk (`.matrixc` format v1..v3)
//! and `OpCode` discriminants stay unchanged so `--prof`, `--coverage`, debugger
//! stepping, `--no-opt`, and `selfhost/vmc.matrix` parity are untouched. On first
//! execution of an uninstrumented chunk, `DecodedChunk::compile` translates the
//! validated instruction stream once into 16-byte fixed-width `DecodedInst`
//! records with direct `pc` jump targets and general multi-opcode fusions.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::value::Value;

use super::bytecode::{operand_len, Chunk, OpCode};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpKind {
    Lt = 0,
    Le = 1,
    Gt = 2,
    Ge = 3,
    Eq = 4,
    Ne = 5,
}

impl CmpKind {
    #[inline]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => CmpKind::Lt,
            1 => CmpKind::Le,
            2 => CmpKind::Gt,
            3 => CmpKind::Ge,
            4 => CmpKind::Eq,
            _ => CmpKind::Ne,
        }
    }

    #[inline(always)]
    pub fn eval_i64(self, a: i64, b: i64) -> bool {
        match self {
            CmpKind::Lt => a < b,
            CmpKind::Le => a <= b,
            CmpKind::Gt => a > b,
            CmpKind::Ge => a >= b,
            CmpKind::Eq => a == b,
            CmpKind::Ne => a != b,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinKind {
    Add = 0,
    Sub = 1,
    Mul = 2,
    Div = 3,
    Mod = 4,
}

impl BinKind {
    #[inline]
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => BinKind::Add,
            1 => BinKind::Sub,
            2 => BinKind::Mul,
            3 => BinKind::Div,
            _ => BinKind::Mod,
        }
    }

    #[inline(always)]
    pub fn eval_i64(self, a: i64, b: i64) -> Option<i64> {
        match self {
            BinKind::Add => a.checked_add(b),
            BinKind::Sub => a.checked_sub(b),
            BinKind::Mul => a.checked_mul(b),
            BinKind::Div => {
                if b != 0 {
                    a.checked_div(b)
                } else {
                    None
                }
            }
            BinKind::Mod => {
                if b != 0 {
                    Some(a.wrapping_rem(b))
                } else {
                    None
                }
            }
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodedOp {
    /// `GetLocal src; Const int_c; Add; SetLocal dst; Pop`
    /// `a = dst`, `b = src`, `c = ic_idx`.
    AddLocalConst,
    /// `GetLocal src; Const int_c; Sub; SetLocal dst; Pop`
    /// `a = dst`, `b = src`, `c = ic_idx`.
    SubLocalConst,
    /// `GetLocal slot; Const step; Add; SetLocal slot; Pop; [0..=4 Pop]; GetLocal slot; (GetLocal | Const) lim; Cmp; JumpIf* target`
    /// `a = slot`, `b = step_ic`, `c = lim_slot_or_ic`, `d = target_pc`,
    /// `aux = (lim_is_const: bit0) | (is_jtrue: bit1) | ((cmp as u8) << 2)`, `ext = pops`.
    IncCmpJumpLocal,
    /// `GetLocal slot; Const step; Add; SetLocal slot; Pop; [0..=4 Pop]; GetLocal slot; GetLocal slot; Mul; GetLocal lim; LessEq; JumpIf* target`
    /// `a = slot`, `b = step_ic`, `c = lim_slot`, `d = target_pc`,
    /// `aux = is_jtrue as u8`, `ext = pops`.
    IncMulCmpJumpLocal,
    /// `GetLocal a; GetLocal b; Cmp; JumpIf* target`
    /// `a = slot_a`, `b = slot_b`, `d = target_pc`,
    /// `aux = (is_jtrue: bit0) | ((cmp as u8) << 1)`.
    CmpJumpLocal2,
    /// `GetLocal a; Const int_c; Cmp; JumpIf* target`
    /// `a = slot_a`, `b = ic_idx`, `d = target_pc`,
    /// `aux = (is_jtrue: bit0) | ((cmp as u8) << 1)`.
    CmpJumpLocalConst,
    /// `GetLocal a; GetLocal b; Mul; GetLocal c; LessEq; JumpIf* target`
    /// `a = slot_a`, `b = slot_b`, `c = slot_c`, `d = target_pc`, `aux = is_jtrue as u8`.
    MulCmpJumpLocal3,
    /// `GetLocal a; GetLocal b; Mod; Const int_k; Equal|NotEqual; JumpIf* target`
    /// `a = slot_a`, `b = slot_b`, `c = ic_idx`, `d = target_pc`, `aux = (is_jtrue: bit0) | (is_ne: bit1)`.
    ModEqZeroJump,
    /// `GetLocal container; GetLocal idx; IndexGet`
    /// `a = container_slot`, `b = idx_slot`.
    IndexGetLocal2,
    /// `GetLocal container; Constant cidx; IndexGet`
    /// `a = container_slot`, `b = cidx`, `c = str_key_idx` (if `aux == 1`), `aux = 1 (str) | 2 (int)`.
    IndexGetLocalConst,
    /// `GetLocal dst; GetLocal la; GetLocal idx; IndexGet; GetLocal lb; GetLocal idx; IndexGet; Mul; Add; SetLocal dst; Pop`
    /// `a = dst`, `b = list_a`, `c = list_b`, `d = idx`.
    AddLocalMulIndex2,
    /// `GetLocal dst; GetLocal container; GetLocal key; IndexGet; Add; SetLocal dst; Pop`
    /// `a = dst`, `b = container`, `c = key`.
    AddLocalIndexGet2,
    /// `GetLocal b; GetLocal c; BinKind; SetLocal dst; Pop`
    /// `a = dst`, `b = slot_b`, `c = slot_c`, `aux = BinKind as u8`.
    BinOpLocal2Set,
    /// `Const str; GetGlobal g; GetLocal var; Const int_m; Mod; Call 1; Add; SetLocal dst; Pop`
    /// `a = dst`, `b = str_cidx`, `c = gidx`, `d = var_slot`, `ext = div_ic_idx`.
    PrefixStrModSet,
    /// Compiled multi-part string template (`a = tpl_idx`, `b = dst_slot` or `u16::MAX` to push).
    EvalStrTemplate,
    /// `GetGlobal gidx; GetLocal arg; Call 1` (`a = gidx`, `b = arg_slot`, `ext = byte_len`).
    CallGlobal1,
    /// `GetLocal obj; Invoke "len" 0` (`a = obj_slot`).
    InvokeLenLocal,
    /// `GetLocal dst; GetLocal obj; Invoke "len" 0; Add; SetLocal dst; Pop` (`a = dst`, `b = obj`).
    AddLocalLen,
    /// `False; Return`
    ReturnFalse,
    /// `True; Return`
    ReturnTrue,
    /// `GetLocal slot; Return` (`a = slot`)
    ReturnLocal,
    /// `Return`
    Return,
    /// `GetLocal slot` (`a = slot`)
    GetLocal,
    /// `SetLocal slot` (`a = slot`)
    SetLocal,
    /// `SetLocal slot; Pop` (`a = slot`)
    SetLocalPop,
    /// `Constant cidx` (`a = cidx`)
    Const,
    /// `Constant int_c` (`a = ic_idx`)
    IntConst,
    /// `Constant cidx; SetLocal slot; Pop` (`a = slot`, `b = cidx`, `ext = ic_idx_or_u16_max`)
    ConstSetLocal,
    /// `Jump target_pc` (`d = target_pc`)
    Jump,
    /// `JumpIfFalse target_pc` (`d = target_pc`)
    JumpIfFalse,
    /// `JumpIfTrue target_pc` (`d = target_pc`)
    JumpIfTrue,
    /// `Loop target_pc` (`d = target_pc`)
    Loop,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Equal,
    NotEqual,
    Less,
    LessEq,
    Greater,
    GreaterEq,
    Pop,
    /// Delegate this original bytecode instruction at `orig_ip` to `run_loop`.
    Fallback,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct DecodedInst {
    pub orig_ip: u32,
    pub op: DecodedOp,
    pub aux: u8,
    pub a: u16,
    pub b: u16,
    pub c: u16,
    pub d: u16,
    pub ext: u16,
}

#[derive(Clone, Debug)]
pub enum StrPartKind {
    Lit(u16),
    Local(u16),
    StringGlobalLocal {
        gidx: u16,
        slot: u16,
    },
    StringGlobalLocalMod {
        gidx: u16,
        slot: u16,
        div_ic: u16,
    },
    StringGlobalLocalMulMod {
        gidx: u16,
        slot: u16,
        mul_ic: u16,
        div_ic: u16,
    },
}

#[derive(Clone, Debug)]
pub struct StrTemplate {
    pub parts: Vec<StrPartKind>,
}

#[derive(Clone, Debug)]
pub struct DecodedChunk {
    pub insts: Vec<DecodedInst>,
    pub ip_to_pc: Vec<u32>,
    pub int_consts: Vec<i64>,
    pub str_templates: Vec<StrTemplate>,
    pub str_keys: Vec<(crate::value::collections::KeyStr, u64)>,
}

#[derive(Clone, Copy)]
struct RawIns {
    op: OpCode,
    off: usize,
    len: usize,
    a_u16: u16,
    a_i16: i16,
    a_u8: u8,
    b_u8: u8,
}

impl RawIns {
    #[inline]
    fn jump_target(self) -> Option<usize> {
        match self.op {
            OpCode::Jump | OpCode::JumpIfFalse | OpCode::JumpIfTrue | OpCode::Loop => {
                let t = self.off as i64 + 3 + self.a_i16 as i64;
                (t >= 0).then_some(t as usize)
            }
            _ => None,
        }
    }
}

#[inline]
fn read_u16_le(code: &[u8], pos: usize) -> u16 {
    u16::from_le_bytes([code[pos], code[pos + 1]])
}

#[inline]
fn read_i16_le(code: &[u8], pos: usize) -> i16 {
    i16::from_le_bytes([code[pos], code[pos + 1]])
}

#[inline]
fn cmp_of(op: OpCode) -> Option<CmpKind> {
    Some(match op {
        OpCode::Less => CmpKind::Lt,
        OpCode::LessEq => CmpKind::Le,
        OpCode::Greater => CmpKind::Gt,
        OpCode::GreaterEq => CmpKind::Ge,
        OpCode::Equal => CmpKind::Eq,
        OpCode::NotEqual => CmpKind::Ne,
        _ => return None,
    })
}

#[inline]
fn bin_of(op: OpCode) -> Option<BinKind> {
    Some(match op {
        OpCode::Add => BinKind::Add,
        OpCode::Sub => BinKind::Sub,
        OpCode::Mul => BinKind::Mul,
        OpCode::Div => BinKind::Div,
        OpCode::Mod => BinKind::Mod,
        _ => return None,
    })
}

#[inline]
fn is_len_const(chunk: &Chunk, cidx: u16) -> bool {
    matches!(
        chunk.constants.get(cidx as usize),
        Some(Value::Str(s)) if s.as_ref() == "len"
    )
}

#[inline]
fn no_internal_targets(raw: &[RawIns], count: usize, targets: &HashSet<usize>) -> bool {
    raw.iter()
        .take(count)
        .skip(1)
        .all(|ins| !targets.contains(&ins.off))
}

impl DecodedChunk {
    /// Decode and fuse a `Chunk`'s validated bytecode once into fixed-width
    /// superinstructions with resolved `pc` jump targets. Returns `None` if
    /// the chunk is malformed, uses non-mappable jump targets, or exceeds `u16`
    /// instruction indexing.
    pub fn compile(chunk: &Chunk) -> Option<Arc<DecodedChunk>> {
        let code = &chunk.code;
        if code.is_empty() {
            return None;
        }
        let mut raw = Vec::with_capacity(code.len() / 2);
        let mut targets: HashSet<usize> = HashSet::new();
        let mut off = 0usize;
        while off < code.len() {
            let op = OpCode::from_u8(code[off])?;
            let oplen = operand_len(op);
            let next = off.checked_add(1 + oplen)?;
            if next > code.len() {
                return None;
            }
            let a_u16 = if oplen >= 2 {
                read_u16_le(code, off + 1)
            } else {
                0
            };
            let a_i16 = if oplen >= 2 {
                read_i16_le(code, off + 1)
            } else {
                0
            };
            let a_u8 = if oplen >= 1 { code[off + 1] } else { 0 };
            let b_u8 = if op == OpCode::Invoke && oplen >= 3 {
                code[off + 3]
            } else {
                0
            };
            let r = RawIns {
                op,
                off,
                len: 1 + oplen,
                a_u16,
                a_i16,
                a_u8,
                b_u8,
            };
            if let Some(t) = r.jump_target() {
                if t >= code.len() {
                    return None;
                }
                targets.insert(t);
            }
            if op == OpCode::Try {
                let catch_t = off as i64 + 7 + read_i16_le(code, off + 3) as i64;
                let finally_t = off as i64 + 7 + read_i16_le(code, off + 5) as i64;
                if catch_t < 0 || finally_t < 0 {
                    return None;
                }
                let (cu, fu) = (catch_t as usize, finally_t as usize);
                if cu >= code.len() || fu >= code.len() {
                    return None;
                }
                targets.insert(cu);
                targets.insert(fu);
            }
            raw.push(r);
            off = next;
        }

        let mut int_consts = Vec::new();
        let mut const_to_ic: Vec<Option<u16>> = vec![None; chunk.constants.len()];
        for (i, c) in chunk.constants.iter().enumerate() {
            if let Value::Int(n) = c {
                let ic = u16::try_from(int_consts.len()).ok()?;
                int_consts.push(*n);
                const_to_ic[i] = Some(ic);
            }
        }
        let get_ic =
            |cidx: u16| -> Option<u16> { const_to_ic.get(cidx as usize).copied().flatten() };

        let mut insts = Vec::with_capacity(raw.len());
        let mut ip_to_pc = vec![u32::MAX; code.len()];
        let mut pending_jumps: Vec<(usize, usize)> = Vec::new();
        let mut str_templates: Vec<StrTemplate> = Vec::new();
        let mut str_keys: Vec<(crate::value::collections::KeyStr, u64)> = Vec::new();

        let mut k = 0usize;
        while k < raw.len() {
            let pc = u16::try_from(insts.len()).ok()? as usize;
            let orig_ip = raw[k].off as u32;
            ip_to_pc[raw[k].off] = pc as u32;

            // 1. AddLocalMulIndex2 (11 instructions):
            //    GetLocal dst; GetLocal la; GetLocal idx; IndexGet; GetLocal lb; GetLocal idx2; IndexGet; Mul; Add; SetLocal dst2; Pop
            if k + 10 < raw.len()
                && raw[k].op == OpCode::GetLocal
                && raw[k + 1].op == OpCode::GetLocal
                && raw[k + 2].op == OpCode::GetLocal
                && raw[k + 3].op == OpCode::IndexGet
                && raw[k + 4].op == OpCode::GetLocal
                && raw[k + 5].op == OpCode::GetLocal
                && raw[k + 6].op == OpCode::IndexGet
                && raw[k + 7].op == OpCode::Mul
                && raw[k + 8].op == OpCode::Add
                && raw[k + 9].op == OpCode::SetLocal
                && raw[k + 10].op == OpCode::Pop
                && raw[k].a_u8 == raw[k + 9].a_u8
                && raw[k + 2].a_u8 == raw[k + 5].a_u8
                && no_internal_targets(&raw[k..], 11, &targets)
            {
                insts.push(DecodedInst {
                    orig_ip,
                    op: DecodedOp::AddLocalMulIndex2,
                    aux: 0,
                    a: raw[k].a_u8 as u16,
                    b: raw[k + 1].a_u8 as u16,
                    c: raw[k + 4].a_u8 as u16,
                    d: raw[k + 2].a_u8 as u16,
                    ext: 0,
                });
                k += 11;
                continue;
            }

            // 2. IncMulCmpJumpLocal / IncCmpJumpLocal (tolerating 0..=4 trailing block Pops):
            //    GetLocal i; Const step; Add; SetLocal i; Pop; [0..=4 Pop]; ...
            if k + 4 < raw.len()
                && raw[k].op == OpCode::GetLocal
                && raw[k + 1].op == OpCode::Constant
                && raw[k + 2].op == OpCode::Add
                && raw[k + 3].op == OpCode::SetLocal
                && raw[k + 4].op == OpCode::Pop
                && raw[k].a_u8 == raw[k + 3].a_u8
            {
                if let Some(step_ic) = get_ic(raw[k + 1].a_u16) {
                    let slot = raw[k].a_u8;
                    let mut pops = 0usize;
                    while pops < 4
                        && k + 5 + pops < raw.len()
                        && raw[k + 5 + pops].op == OpCode::Pop
                    {
                        pops += 1;
                    }
                    let p = k + 5 + pops;
                    // 2a. IncMulCmpJumpLocal:
                    //     GetLocal i; GetLocal i; Mul; GetLocal lim; LessEq; JumpIf* target
                    if p + 5 < raw.len()
                        && raw[p].op == OpCode::GetLocal
                        && raw[p].a_u8 == slot
                        && raw[p + 1].op == OpCode::GetLocal
                        && raw[p + 1].a_u8 == slot
                        && raw[p + 2].op == OpCode::Mul
                        && raw[p + 3].op == OpCode::GetLocal
                        && raw[p + 4].op == OpCode::LessEq
                        && no_internal_targets(&raw[k..], 5 + pops + 6, &targets)
                    {
                        if let Some(target_off) = raw[p + 5].jump_target() {
                            let is_jtrue = raw[p + 5].op == OpCode::JumpIfTrue;
                            if is_jtrue || raw[p + 5].op == OpCode::JumpIfFalse {
                                pending_jumps.push((pc, target_off));
                                insts.push(DecodedInst {
                                    orig_ip,
                                    op: DecodedOp::IncMulCmpJumpLocal,
                                    aux: is_jtrue as u8,
                                    a: slot as u16,
                                    b: step_ic,
                                    c: raw[p + 3].a_u8 as u16,
                                    d: 0,
                                    ext: pops as u16,
                                });
                                k += 5 + pops + 6;
                                continue;
                            }
                        }
                    }
                    // 2b. IncCmpJumpLocal:
                    //     GetLocal i; (GetLocal | Const) lim; Cmp; JumpIf* target
                    if p + 3 < raw.len()
                        && raw[p].op == OpCode::GetLocal
                        && raw[p].a_u8 == slot
                        && no_internal_targets(&raw[k..], 5 + pops + 4, &targets)
                    {
                        if let (Some(cmp), Some(target_off)) =
                            (cmp_of(raw[p + 2].op), raw[p + 3].jump_target())
                        {
                            let is_jtrue = raw[p + 3].op == OpCode::JumpIfTrue;
                            if is_jtrue || raw[p + 3].op == OpCode::JumpIfFalse {
                                let lim_opt = match raw[p + 1].op {
                                    OpCode::GetLocal => Some((false, raw[p + 1].a_u8 as u16)),
                                    OpCode::Constant => {
                                        get_ic(raw[p + 1].a_u16).map(|ic| (true, ic))
                                    }
                                    _ => None,
                                };
                                if let Some((lim_is_const, lim_val)) = lim_opt {
                                    let flags = (lim_is_const as u8)
                                        | ((is_jtrue as u8) << 1)
                                        | ((cmp as u8) << 2);
                                    pending_jumps.push((pc, target_off));
                                    insts.push(DecodedInst {
                                        orig_ip,
                                        op: DecodedOp::IncCmpJumpLocal,
                                        aux: flags,
                                        a: slot as u16,
                                        b: step_ic,
                                        c: lim_val,
                                        d: 0,
                                        ext: pops as u16,
                                    });
                                    k += 5 + pops + 4;
                                    continue;
                                }
                            }
                        }
                    }
                }
            }

            // 3. PrefixStrModSet (7 or 9 instructions):
            //    Const str; GetGlobal g; GetLocal var; Const int_m; Mod; Call 1; Add [; SetLocal dst; Pop]
            if k + 6 < raw.len()
                && raw[k].op == OpCode::Constant
                && matches!(
                    chunk.constants.get(raw[k].a_u16 as usize),
                    Some(Value::Str(_))
                )
                && raw[k + 1].op == OpCode::GetGlobal
                && raw[k + 2].op == OpCode::GetLocal
                && raw[k + 3].op == OpCode::Constant
                && raw[k + 4].op == OpCode::Mod
                && raw[k + 5].op == OpCode::Call
                && raw[k + 5].a_u8 == 1
                && raw[k + 6].op == OpCode::Add
                && no_internal_targets(&raw[k..], 7, &targets)
            {
                if let Some(div_ic) = get_ic(raw[k + 3].a_u16) {
                    let (dst, consumed) = if k + 8 < raw.len()
                        && raw[k + 7].op == OpCode::SetLocal
                        && raw[k + 8].op == OpCode::Pop
                        && !targets.contains(&raw[k + 7].off)
                        && !targets.contains(&raw[k + 8].off)
                    {
                        (raw[k + 7].a_u8 as u16, 9)
                    } else {
                        (u16::MAX, 7)
                    };
                    insts.push(DecodedInst {
                        orig_ip,
                        op: DecodedOp::PrefixStrModSet,
                        aux: 0,
                        a: dst,
                        b: raw[k].a_u16,
                        c: raw[k + 1].a_u16,
                        d: raw[k + 2].a_u8 as u16,
                        ext: div_ic,
                    });
                    k += consumed;
                    continue;
                }
            }

            // 4. General multi-part string template expression (concatenated with `+`):
            if let Some((tpl, consumed, dst_opt)) =
                match_str_template(&raw[k..], chunk, &get_ic, &targets)
            {
                if let Ok(tpl_idx) = u16::try_from(str_templates.len()) {
                    str_templates.push(tpl);
                    insts.push(DecodedInst {
                        orig_ip,
                        op: DecodedOp::EvalStrTemplate,
                        aux: 0,
                        a: tpl_idx,
                        b: dst_opt.unwrap_or(u16::MAX),
                        c: 0,
                        d: 0,
                        ext: 0,
                    });
                    k += consumed;
                    continue;
                }
            }

            // 5. AddLocalIndexGet2 (7 instructions):
            //    GetLocal dst; GetLocal container; GetLocal key; IndexGet; Add; SetLocal dst; Pop
            if k + 6 < raw.len()
                && raw[k].op == OpCode::GetLocal
                && raw[k + 1].op == OpCode::GetLocal
                && raw[k + 2].op == OpCode::GetLocal
                && raw[k + 3].op == OpCode::IndexGet
                && raw[k + 4].op == OpCode::Add
                && raw[k + 5].op == OpCode::SetLocal
                && raw[k + 6].op == OpCode::Pop
                && raw[k].a_u8 == raw[k + 5].a_u8
                && no_internal_targets(&raw[k..], 7, &targets)
            {
                insts.push(DecodedInst {
                    orig_ip,
                    op: DecodedOp::AddLocalIndexGet2,
                    aux: 0,
                    a: raw[k].a_u8 as u16,
                    b: raw[k + 1].a_u8 as u16,
                    c: raw[k + 2].a_u8 as u16,
                    d: 0,
                    ext: 0,
                });
                k += 7;
                continue;
            }

            // 6. AddLocalLen (6 instructions):
            //    GetLocal dst; GetLocal obj; Invoke "len" 0; Add; SetLocal dst; Pop
            if k + 5 < raw.len()
                && raw[k].op == OpCode::GetLocal
                && raw[k + 1].op == OpCode::GetLocal
                && raw[k + 2].op == OpCode::Invoke
                && raw[k + 2].b_u8 == 0
                && is_len_const(chunk, raw[k + 2].a_u16)
                && raw[k + 3].op == OpCode::Add
                && raw[k + 4].op == OpCode::SetLocal
                && raw[k + 5].op == OpCode::Pop
                && raw[k].a_u8 == raw[k + 4].a_u8
                && no_internal_targets(&raw[k..], 6, &targets)
            {
                insts.push(DecodedInst {
                    orig_ip,
                    op: DecodedOp::AddLocalLen,
                    aux: 0,
                    a: raw[k].a_u8 as u16,
                    b: raw[k + 1].a_u8 as u16,
                    c: 0,
                    d: 0,
                    ext: 0,
                });
                k += 6;
                continue;
            }

            // 7. MulCmpJumpLocal3 (6 instructions):
            //    GetLocal a; GetLocal b; Mul; GetLocal c; LessEq; JumpIf* target
            if k + 5 < raw.len()
                && raw[k].op == OpCode::GetLocal
                && raw[k + 1].op == OpCode::GetLocal
                && raw[k + 2].op == OpCode::Mul
                && raw[k + 3].op == OpCode::GetLocal
                && raw[k + 4].op == OpCode::LessEq
                && no_internal_targets(&raw[k..], 6, &targets)
            {
                if let Some(target_off) = raw[k + 5].jump_target() {
                    let is_jtrue = raw[k + 5].op == OpCode::JumpIfTrue;
                    if is_jtrue || raw[k + 5].op == OpCode::JumpIfFalse {
                        pending_jumps.push((pc, target_off));
                        insts.push(DecodedInst {
                            orig_ip,
                            op: DecodedOp::MulCmpJumpLocal3,
                            aux: is_jtrue as u8,
                            a: raw[k].a_u8 as u16,
                            b: raw[k + 1].a_u8 as u16,
                            c: raw[k + 3].a_u8 as u16,
                            d: 0,
                            ext: 0,
                        });
                        k += 6;
                        continue;
                    }
                }
            }

            // 8. ModEqZeroJump (6 instructions):
            //    GetLocal a; GetLocal b; Mod; Const int_k; Equal|NotEqual; JumpIf* target
            if k + 5 < raw.len()
                && raw[k].op == OpCode::GetLocal
                && raw[k + 1].op == OpCode::GetLocal
                && raw[k + 2].op == OpCode::Mod
                && raw[k + 3].op == OpCode::Constant
                && (raw[k + 4].op == OpCode::Equal || raw[k + 4].op == OpCode::NotEqual)
                && no_internal_targets(&raw[k..], 6, &targets)
            {
                if let (Some(k_ic), Some(target_off)) =
                    (get_ic(raw[k + 3].a_u16), raw[k + 5].jump_target())
                {
                    let is_jtrue = raw[k + 5].op == OpCode::JumpIfTrue;
                    if is_jtrue || raw[k + 5].op == OpCode::JumpIfFalse {
                        let is_ne = raw[k + 4].op == OpCode::NotEqual;
                        pending_jumps.push((pc, target_off));
                        insts.push(DecodedInst {
                            orig_ip,
                            op: DecodedOp::ModEqZeroJump,
                            aux: (is_jtrue as u8) | ((is_ne as u8) << 1),
                            a: raw[k].a_u8 as u16,
                            b: raw[k + 1].a_u8 as u16,
                            c: k_ic,
                            d: 0,
                            ext: 0,
                        });
                        k += 6;
                        continue;
                    }
                }
            }

            // 9. AddLocalConst / SubLocalConst / BinOpLocal2Set (5 instructions)
            if k + 4 < raw.len()
                && raw[k].op == OpCode::GetLocal
                && raw[k + 3].op == OpCode::SetLocal
                && raw[k + 4].op == OpCode::Pop
                && no_internal_targets(&raw[k..], 5, &targets)
            {
                if raw[k + 1].op == OpCode::Constant
                    && (raw[k + 2].op == OpCode::Add || raw[k + 2].op == OpCode::Sub)
                {
                    if let Some(c_ic) = get_ic(raw[k + 1].a_u16) {
                        insts.push(DecodedInst {
                            orig_ip,
                            op: if raw[k + 2].op == OpCode::Add {
                                DecodedOp::AddLocalConst
                            } else {
                                DecodedOp::SubLocalConst
                            },
                            aux: 0,
                            a: raw[k + 3].a_u8 as u16,
                            b: raw[k].a_u8 as u16,
                            c: c_ic,
                            d: 0,
                            ext: 0,
                        });
                        k += 5;
                        continue;
                    }
                } else if raw[k + 1].op == OpCode::GetLocal {
                    if let Some(bk) = bin_of(raw[k + 2].op) {
                        insts.push(DecodedInst {
                            orig_ip,
                            op: DecodedOp::BinOpLocal2Set,
                            aux: bk as u8,
                            a: raw[k + 3].a_u8 as u16,
                            b: raw[k].a_u8 as u16,
                            c: raw[k + 1].a_u8 as u16,
                            d: 0,
                            ext: 0,
                        });
                        k += 5;
                        continue;
                    }
                }
            }

            // 10. CmpJumpLocal2 / CmpJumpLocalConst (4 instructions)
            if k + 3 < raw.len()
                && raw[k].op == OpCode::GetLocal
                && no_internal_targets(&raw[k..], 4, &targets)
            {
                if let (Some(cmp), Some(target_off)) =
                    (cmp_of(raw[k + 2].op), raw[k + 3].jump_target())
                {
                    let is_jtrue = raw[k + 3].op == OpCode::JumpIfTrue;
                    if is_jtrue || raw[k + 3].op == OpCode::JumpIfFalse {
                        let flags = (is_jtrue as u8) | ((cmp as u8) << 1);
                        if raw[k + 1].op == OpCode::GetLocal {
                            pending_jumps.push((pc, target_off));
                            insts.push(DecodedInst {
                                orig_ip,
                                op: DecodedOp::CmpJumpLocal2,
                                aux: flags,
                                a: raw[k].a_u8 as u16,
                                b: raw[k + 1].a_u8 as u16,
                                c: 0,
                                d: 0,
                                ext: 0,
                            });
                            k += 4;
                            continue;
                        } else if raw[k + 1].op == OpCode::Constant {
                            if let Some(c_ic) = get_ic(raw[k + 1].a_u16) {
                                pending_jumps.push((pc, target_off));
                                insts.push(DecodedInst {
                                    orig_ip,
                                    op: DecodedOp::CmpJumpLocalConst,
                                    aux: flags,
                                    a: raw[k].a_u8 as u16,
                                    b: c_ic,
                                    c: 0,
                                    d: 0,
                                    ext: 0,
                                });
                                k += 4;
                                continue;
                            }
                        }
                    }
                }
            }

            // 11. CallGlobal1 (3 instructions): GetGlobal g; GetLocal arg; Call 1
            if k + 2 < raw.len()
                && raw[k].op == OpCode::GetGlobal
                && raw[k + 1].op == OpCode::GetLocal
                && raw[k + 2].op == OpCode::Call
                && raw[k + 2].a_u8 == 1
                && no_internal_targets(&raw[k..], 3, &targets)
            {
                let byte_len = (raw[k].len + raw[k + 1].len + raw[k + 2].len) as u16;
                insts.push(DecodedInst {
                    orig_ip,
                    op: DecodedOp::CallGlobal1,
                    aux: 0,
                    a: raw[k].a_u16,
                    b: raw[k + 1].a_u8 as u16,
                    c: 0,
                    d: 0,
                    ext: byte_len,
                });
                k += 3;
                continue;
            }

            // 12. IndexGetLocal2 (3 instructions): GetLocal container; GetLocal idx; IndexGet
            if k + 2 < raw.len()
                && raw[k].op == OpCode::GetLocal
                && raw[k + 1].op == OpCode::GetLocal
                && raw[k + 2].op == OpCode::IndexGet
                && no_internal_targets(&raw[k..], 3, &targets)
            {
                insts.push(DecodedInst {
                    orig_ip,
                    op: DecodedOp::IndexGetLocal2,
                    aux: 0,
                    a: raw[k].a_u8 as u16,
                    b: raw[k + 1].a_u8 as u16,
                    c: 0,
                    d: 0,
                    ext: 0,
                });
                k += 3;
                continue;
            }

            // 12b. IndexGetLocalConst (3 instructions): GetLocal container; Constant cidx; IndexGet
            if k + 2 < raw.len()
                && raw[k].op == OpCode::GetLocal
                && raw[k + 1].op == OpCode::Constant
                && raw[k + 2].op == OpCode::IndexGet
                && no_internal_targets(&raw[k..], 3, &targets)
            {
                let cidx = raw[k + 1].a_u16;
                match chunk.constants.get(cidx as usize) {
                    Some(Value::Str(s)) => {
                        if let Ok(key_idx) = u16::try_from(str_keys.len()) {
                            let h = crate::value::collections::fx_hash_str(s.as_ref());
                            let ks = crate::value::collections::intern_key_with_hash(s.as_ref(), h);
                            str_keys.push((ks, h));
                            insts.push(DecodedInst {
                                orig_ip,
                                op: DecodedOp::IndexGetLocalConst,
                                aux: 1,
                                a: raw[k].a_u8 as u16,
                                b: cidx,
                                c: key_idx,
                                d: 0,
                                ext: 0,
                            });
                            k += 3;
                            continue;
                        }
                    }
                    Some(Value::Int(_)) => {
                        insts.push(DecodedInst {
                            orig_ip,
                            op: DecodedOp::IndexGetLocalConst,
                            aux: 2,
                            a: raw[k].a_u8 as u16,
                            b: cidx,
                            c: 0,
                            d: 0,
                            ext: 0,
                        });
                        k += 3;
                        continue;
                    }
                    _ => {}
                }
            }

            // 13. ConstSetLocal (3 instructions): Constant c; SetLocal dst; Pop
            if k + 2 < raw.len()
                && raw[k].op == OpCode::Constant
                && raw[k + 1].op == OpCode::SetLocal
                && raw[k + 2].op == OpCode::Pop
                && no_internal_targets(&raw[k..], 3, &targets)
            {
                insts.push(DecodedInst {
                    orig_ip,
                    op: DecodedOp::ConstSetLocal,
                    aux: 0,
                    a: raw[k + 1].a_u8 as u16,
                    b: raw[k].a_u16,
                    c: 0,
                    d: 0,
                    ext: get_ic(raw[k].a_u16).unwrap_or(u16::MAX),
                });
                k += 3;
                continue;
            }

            // 14. 2-instruction pairs: InvokeLenLocal, ReturnFalse/True/Local, SetLocalPop
            if k + 1 < raw.len() && no_internal_targets(&raw[k..], 2, &targets) {
                if raw[k].op == OpCode::GetLocal
                    && raw[k + 1].op == OpCode::Invoke
                    && raw[k + 1].b_u8 == 0
                    && is_len_const(chunk, raw[k + 1].a_u16)
                {
                    insts.push(DecodedInst {
                        orig_ip,
                        op: DecodedOp::InvokeLenLocal,
                        aux: 0,
                        a: raw[k].a_u8 as u16,
                        b: 0,
                        c: 0,
                        d: 0,
                        ext: 0,
                    });
                    k += 2;
                    continue;
                }
                if raw[k].op == OpCode::False && raw[k + 1].op == OpCode::Return {
                    insts.push(DecodedInst {
                        orig_ip,
                        op: DecodedOp::ReturnFalse,
                        aux: 0,
                        a: 0,
                        b: 0,
                        c: 0,
                        d: 0,
                        ext: 0,
                    });
                    k += 2;
                    continue;
                }
                if raw[k].op == OpCode::True && raw[k + 1].op == OpCode::Return {
                    insts.push(DecodedInst {
                        orig_ip,
                        op: DecodedOp::ReturnTrue,
                        aux: 0,
                        a: 0,
                        b: 0,
                        c: 0,
                        d: 0,
                        ext: 0,
                    });
                    k += 2;
                    continue;
                }
                if raw[k].op == OpCode::GetLocal && raw[k + 1].op == OpCode::Return {
                    insts.push(DecodedInst {
                        orig_ip,
                        op: DecodedOp::ReturnLocal,
                        aux: 0,
                        a: raw[k].a_u8 as u16,
                        b: 0,
                        c: 0,
                        d: 0,
                        ext: 0,
                    });
                    k += 2;
                    continue;
                }
                if raw[k].op == OpCode::SetLocal && raw[k + 1].op == OpCode::Pop {
                    insts.push(DecodedInst {
                        orig_ip,
                        op: DecodedOp::SetLocalPop,
                        aux: 0,
                        a: raw[k].a_u8 as u16,
                        b: 0,
                        c: 0,
                        d: 0,
                        ext: 0,
                    });
                    k += 2;
                    continue;
                }
            }

            // 15. Single instruction mappings
            let single = match raw[k].op {
                OpCode::GetLocal => DecodedInst {
                    orig_ip,
                    op: DecodedOp::GetLocal,
                    aux: 0,
                    a: raw[k].a_u8 as u16,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::SetLocal => DecodedInst {
                    orig_ip,
                    op: DecodedOp::SetLocal,
                    aux: 0,
                    a: raw[k].a_u8 as u16,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::Constant => {
                    if let Some(ic) = get_ic(raw[k].a_u16) {
                        DecodedInst {
                            orig_ip,
                            op: DecodedOp::IntConst,
                            aux: 0,
                            a: ic,
                            b: raw[k].a_u16,
                            c: 0,
                            d: 0,
                            ext: 0,
                        }
                    } else {
                        DecodedInst {
                            orig_ip,
                            op: DecodedOp::Const,
                            aux: 0,
                            a: raw[k].a_u16,
                            b: 0,
                            c: 0,
                            d: 0,
                            ext: 0,
                        }
                    }
                }
                OpCode::Jump | OpCode::JumpIfFalse | OpCode::JumpIfTrue | OpCode::Loop => {
                    let target_off = raw[k].jump_target()?;
                    pending_jumps.push((pc, target_off));
                    let op = match raw[k].op {
                        OpCode::Jump => DecodedOp::Jump,
                        OpCode::JumpIfFalse => DecodedOp::JumpIfFalse,
                        OpCode::JumpIfTrue => DecodedOp::JumpIfTrue,
                        _ => DecodedOp::Loop,
                    };
                    DecodedInst {
                        orig_ip,
                        op,
                        aux: 0,
                        a: 0,
                        b: 0,
                        c: 0,
                        d: 0,
                        ext: 0,
                    }
                }
                OpCode::Add => DecodedInst {
                    orig_ip,
                    op: DecodedOp::Add,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::Sub => DecodedInst {
                    orig_ip,
                    op: DecodedOp::Sub,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::Mul => DecodedInst {
                    orig_ip,
                    op: DecodedOp::Mul,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::Div => DecodedInst {
                    orig_ip,
                    op: DecodedOp::Div,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::Mod => DecodedInst {
                    orig_ip,
                    op: DecodedOp::Mod,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::Equal => DecodedInst {
                    orig_ip,
                    op: DecodedOp::Equal,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::NotEqual => DecodedInst {
                    orig_ip,
                    op: DecodedOp::NotEqual,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::Less => DecodedInst {
                    orig_ip,
                    op: DecodedOp::Less,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::LessEq => DecodedInst {
                    orig_ip,
                    op: DecodedOp::LessEq,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::Greater => DecodedInst {
                    orig_ip,
                    op: DecodedOp::Greater,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::GreaterEq => DecodedInst {
                    orig_ip,
                    op: DecodedOp::GreaterEq,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::Pop => DecodedInst {
                    orig_ip,
                    op: DecodedOp::Pop,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                OpCode::Return => DecodedInst {
                    orig_ip,
                    op: DecodedOp::Return,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
                _ => DecodedInst {
                    orig_ip,
                    op: DecodedOp::Fallback,
                    aux: 0,
                    a: 0,
                    b: 0,
                    c: 0,
                    d: 0,
                    ext: 0,
                },
            };
            insts.push(single);
            k += 1;
        }

        // Resolve all jump targets to decoded `pc` indices.
        for (pc, target_off) in pending_jumps {
            let mapped = *ip_to_pc.get(target_off)?;
            if mapped == u32::MAX || mapped as usize >= insts.len() {
                return None;
            }
            insts[pc].d = mapped as u16;
        }

        // Record orig_ip -> pc for return sites of any Fallback Call/Invoke too
        // (all single ops and CallGlobal1 next instructions were already mapped).
        let _ = HashMap::<usize, usize>::new;

        Some(Arc::new(DecodedChunk {
            insts,
            ip_to_pc,
            int_consts,
            str_templates,
            str_keys,
        }))
    }
}

fn match_str_template(
    raw: &[RawIns],
    chunk: &Chunk,
    get_ic: &dyn Fn(u16) -> Option<u16>,
    targets: &HashSet<usize>,
) -> Option<(StrTemplate, usize, Option<u16>)> {
    if raw.len() < 4 || raw[0].op != OpCode::Constant {
        return None;
    }
    if !matches!(
        chunk.constants.get(raw[0].a_u16 as usize),
        Some(Value::Str(_))
    ) {
        return None;
    }
    let mut parts = vec![StrPartKind::Lit(raw[0].a_u16)];
    let mut pos = 1usize;

    while pos + 1 < raw.len() {
        // Parse `[part_ops..., Add]` without crossing any jump target.
        let part_res: Option<(StrPartKind, usize)> = if raw[pos].op == OpCode::Constant
            && matches!(
                chunk.constants.get(raw[pos].a_u16 as usize),
                Some(Value::Str(_))
            )
            && raw[pos + 1].op == OpCode::Add
        {
            Some((StrPartKind::Lit(raw[pos].a_u16), pos + 2))
        } else if raw[pos].op == OpCode::GetLocal && raw[pos + 1].op == OpCode::Add {
            Some((StrPartKind::Local(raw[pos].a_u8 as u16), pos + 2))
        } else if pos + 3 < raw.len()
            && raw[pos].op == OpCode::GetGlobal
            && raw[pos + 1].op == OpCode::GetLocal
            && raw[pos + 2].op == OpCode::Call
            && raw[pos + 2].a_u8 == 1
            && raw[pos + 3].op == OpCode::Add
        {
            Some((
                StrPartKind::StringGlobalLocal {
                    gidx: raw[pos].a_u16,
                    slot: raw[pos + 1].a_u8 as u16,
                },
                pos + 4,
            ))
        } else if pos + 5 < raw.len()
            && raw[pos].op == OpCode::GetGlobal
            && raw[pos + 1].op == OpCode::GetLocal
            && raw[pos + 2].op == OpCode::Constant
            && raw[pos + 3].op == OpCode::Mod
            && raw[pos + 4].op == OpCode::Call
            && raw[pos + 4].a_u8 == 1
            && raw[pos + 5].op == OpCode::Add
        {
            get_ic(raw[pos + 2].a_u16).map(|div_ic| {
                (
                    StrPartKind::StringGlobalLocalMod {
                        gidx: raw[pos].a_u16,
                        slot: raw[pos + 1].a_u8 as u16,
                        div_ic,
                    },
                    pos + 6,
                )
            })
        } else if pos + 7 < raw.len()
            && raw[pos].op == OpCode::GetGlobal
            && raw[pos + 1].op == OpCode::GetLocal
            && raw[pos + 2].op == OpCode::Constant
            && raw[pos + 3].op == OpCode::Mul
            && raw[pos + 4].op == OpCode::Constant
            && raw[pos + 5].op == OpCode::Mod
            && raw[pos + 6].op == OpCode::Call
            && raw[pos + 6].a_u8 == 1
            && raw[pos + 7].op == OpCode::Add
        {
            match (get_ic(raw[pos + 2].a_u16), get_ic(raw[pos + 4].a_u16)) {
                (Some(mul_ic), Some(div_ic)) => Some((
                    StrPartKind::StringGlobalLocalMulMod {
                        gidx: raw[pos].a_u16,
                        slot: raw[pos + 1].a_u8 as u16,
                        mul_ic,
                        div_ic,
                    },
                    pos + 8,
                )),
                _ => None,
            }
        } else {
            None
        };

        let Some((part, next_pos)) = part_res else {
            break;
        };
        if !no_internal_targets(&raw[pos..], next_pos - pos, targets) {
            break;
        }
        // Also ensure `pos` itself (inside the whole template) is not a target.
        if targets.contains(&raw[pos].off) {
            break;
        }
        parts.push(part);
        pos = next_pos;
    }

    if parts.len() < 3 {
        return None;
    }

    // Consume optional `SetLocal dst; Pop` when untargeted.
    let mut dst_opt = None;
    if pos + 1 < raw.len()
        && !targets.contains(&raw[pos].off)
        && !targets.contains(&raw[pos + 1].off)
        && raw[pos].op == OpCode::SetLocal
        && raw[pos + 1].op == OpCode::Pop
    {
        dst_opt = Some(raw[pos].a_u8 as u16);
        pos += 2;
    }

    Some((StrTemplate { parts }, pos, dst_opt))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::parser;
    use crate::semantic;
    use crate::vm::compiler::{compile_program, compile_program_unoptimized};
    use crate::vm::Vm;

    #[test]
    fn decoded_chunk_preserves_jump_targets_and_fuses_loops() {
        let src = r#"
fn main() {
    let s = 0;
    let i = 0;
    while i < 10 {
        s = s + i;
        i += 1;
    }
    return s;
}
"#;
        let prog = parser::parse_file(src, Arc::from("t.matrix")).unwrap();
        semantic::check_file(&prog, true, "t.matrix").unwrap();
        let (func, _) = compile_program(&prog, "t.matrix", &[]).unwrap();
        let main_fn = func
            .chunk
            .functions
            .iter()
            .find(|f| f.name.as_deref() == Some("main"))
            .expect("main proto");
        let dec = DecodedChunk::compile(&main_fn.chunk).expect("decode succeeds");
        assert!(
            dec.insts
                .iter()
                .any(|i| matches!(i.op, DecodedOp::IncCmpJumpLocal | DecodedOp::BinOpLocal2Set)),
            "expected loop superinstructions in {:?}",
            dec.insts.iter().map(|i| i.op).collect::<Vec<_>>()
        );
        for inst in &dec.insts {
            if matches!(
                inst.op,
                DecodedOp::IncCmpJumpLocal
                    | DecodedOp::CmpJumpLocal2
                    | DecodedOp::CmpJumpLocalConst
                    | DecodedOp::MulCmpJumpLocal3
                    | DecodedOp::ModEqZeroJump
                    | DecodedOp::Jump
                    | DecodedOp::JumpIfFalse
                    | DecodedOp::JumpIfTrue
                    | DecodedOp::Loop
            ) {
                assert!((inst.d as usize) < dec.insts.len());
            }
        }
    }

    #[test]
    fn decoded_vm_matches_no_opt_on_overflow_div0_and_aliasing() {
        let src = r#"
fn is_prime(n) {
    if n < 2 { return false; }
    let d = 2;
    while d * d <= n {
        if n % d == 0 { return false; }
        d += 1;
    }
    return true;
}

fn dot_self(a) {
    let s = 0;
    let j = 0;
    let n = a.len();
    while j < n {
        s = s + a[j] * a[j];
        j += 1;
    }
    return s;
}

fn main() {
    let primes = 0;
    let k = 2;
    while k < 30 {
        if is_prime(k) { primes += 1; }
        k += 1;
    }
    let xs = [1, 2, 3, 4];
    let ds = dot_self(xs);
    let errs = 0;
    try {
        let x = 9223372036854775807;
        let y = x + 1;
        println(y);
    } catch (e) {
        errs += 1;
    }
    try {
        let a = 10;
        let b = 0;
        println(a / b);
    } catch (e) {
        errs += 1;
    }
    try {
        let arr = [1, 2];
        let z = 0;
        let idx = 5;
        z = z + arr[idx] * arr[idx];
        println(z);
    } catch (e) {
        errs += 1;
    }
    println(primes, ds, errs);
    return primes + ds + errs;
}
"#;
        let prog = parser::parse_file(src, Arc::from("parity.matrix")).unwrap();
        semantic::check_file(&prog, true, "parity.matrix").unwrap();
        let (opt_f, _) = compile_program(&prog, "parity.matrix", &[]).unwrap();
        let (raw_f, _) = compile_program_unoptimized(&prog, "parity.matrix", &[]).unwrap();
        let mut vm_opt = Vm::new(Arc::from("parity.matrix"), Vec::new());
        let mut vm_noopt = Vm::new(Arc::from("parity.matrix"), Vec::new());
        vm_noopt.set_opt_enabled(false);
        let r_opt = vm_opt.run_compiled(Arc::new(opt_f)).unwrap();
        let r_noopt = vm_noopt.run_compiled(Arc::new(raw_f)).unwrap();
        assert_eq!(r_opt, r_noopt);
        assert_eq!(r_opt, 10 + 30 + 3);
    }
}
