//! AST lowering: the single seam between surface syntax and the backends.
//!
//! Every backend — the tree interpreter, the bytecode VM and the LLVM AOT
//! backend — consumes the AST produced here, never the parser's output
//! directly. Today the pass is the **identity**; it exists so that a *syntax*
//! feature can be written once as a desugaring instead of three times as an
//! engine feature (the roadmap's Stage 2/3: `?.`, `??`, string interpolation,
//! destructuring, expression `if`/blocks, pattern matching).
//!
//! Contract:
//!
//! * **Idempotent.** A backend may be handed an already-lowered program — the VM
//!   compiles module programs through the same entry point as the main one.
//! * **Span-preserving.** Diagnostics and stack traces must keep pointing at the
//!   source the user wrote, so a synthesized node carries the span of the
//!   construct it replaces.
//! * **Behaviour-preserving.** `tests/engine_matrix.rs` is the proof: it runs
//!   every statement and expression form on every engine and compares the output
//!   against the expected text, so a lowering rule that changes meaning fails
//!   there rather than in a user's program.
//!
//! Returning `Cow` keeps the identity case allocation-free: the backends get a
//! borrow today, and only a rule that actually rewrites pays for a clone.

use std::borrow::Cow;

use crate::ast::Program;

/// Lower a parsed program into the form the backends execute.
pub fn lower(program: &Program) -> Cow<'_, Program> {
    Cow::Borrowed(program)
}

/// Lower an owned program, cloning only if a rule fires.
pub fn lower_owned(program: Program) -> Program {
    match lower(&program) {
        Cow::Borrowed(_) => program,
        Cow::Owned(rewritten) => rewritten,
    }
}
