//! Native backend: compile Spid3r programs to real machine code.
//!
//! Two halves:
//! - [`rt`] — the runtime: `extern "C"` trampolines + `spid3r_rt_start`.
//!   These live inside the `staticlib` we produce (`libspid3r.a` / `spid3r.lib`)
//!   and are what AOT-compiled binaries call into. Zero dependencies.
//! - `ir` / `link` (behind the `native` feature) — LLVM IR generation via
//!   inkwell, object emission, and final linking of the executable.

#![allow(dead_code)] // trampolines are used by *generated* code, not always by this crate

pub mod rt;

#[cfg(feature = "native")]
pub mod ir;

#[cfg(feature = "native")]
pub mod link;

#[cfg(feature = "native")]
pub mod modules;

#[cfg(feature = "native")]
pub mod llvm;
