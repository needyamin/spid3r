//! Spid3rOS bare-metal kernel library (`R13` / `DP-8`).
//!
//! Pure, zero-dependency kernel subsystems designed to compile under both
//! `x86_64-unknown-none` (`no_std`) and host `cargo test` for deterministic verification.

#![cfg_attr(target_os = "none", no_std)]

pub mod boot;
pub mod console;
pub mod drivers;
pub mod intr;
pub mod mem;
pub mod sched;
pub mod user;
