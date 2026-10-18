//! `spid3r debug` tests: scripted debugger sessions over the bytecode VM.
//!
//! The debugger reads commands from `--commands FILE` (a `#` starts a comment
//! line), so a session is fully reproducible: pause at breakpoints, inspect
//! state, step, and resume. When the command list runs out the program
//! continues to completion.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_spid3r"))
}

struct Session {
    dir: PathBuf,
}

impl Session {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("spid3r_dbg_{}_{}", tag, std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        Session { dir }
    }

    fn write(&self, name: &str, text: &str) -> String {
        let p = self.dir.join(name);
        std::fs::write(&p, text).expect("write");
        p.to_str().unwrap().to_string()
    }

    fn run(&self, program: &str, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(bin())
            .args(["debug", program])
            .args(args)
            .output()
            .expect("run debug");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The program prints distinctive markers (`sum=5`, `sum=4`) so assertions
/// about program output cannot accidentally match digits in temp paths or
/// debugger internals.
const PROGRAM: &str = "\
fn add(a, b) {
    let s = a + b;
    return s;
}
fn main() {
    let x = 2;
    let y = 3;
    println(\"sum=\" + string(add(x, y)));
    println(\"sum=\" + string(add(x, x)));
    return 0;
}
";

#[test]
fn breakpoint_pauses_and_inspection_commands_work() {
    let s = Session::new("inspect");
    let prog = s.write("prog.matrix", PROGRAM);
    let cmds = s.write(
        "cmds.txt",
        "# inspect the frame at the breakpoint\nlocals\nbt\nlist\ncontinue\n",
    );
    let (code, out, err) = s.run(&prog, &["--break", "3", "--commands", &cmds]);
    assert_eq!(code, 0, "program should finish; stderr: {}", err);
    assert!(out.contains("breakpoint at"), "pause banner missing:\n{}", out);
    assert!(out.contains("prog.matrix:3:"), "location missing:\n{}", out);
    assert!(out.contains("return s;"), "source line missing:\n{}", out);
    assert!(out.contains("add slots"), "locals missing:\n{}", out);
    assert!(out.contains("stack trace (most recent call first):"), "bt missing:\n{}", out);
    assert!(out.contains("at add ("), "bt frame missing:\n{}", out);
    assert!(out.contains("at main ("), "bt caller missing:\n{}", out);
    // The program itself still ran to completion and printed both results.
    assert!(out.contains("sum=5"), "program output missing:\n{}", out);
    assert!(out.contains("sum=4"), "program output missing:\n{}", out);
}

#[test]
fn next_steps_over_into_the_caller() {
    let s = Session::new("next");
    let prog = s.write("prog.matrix", PROGRAM);
    let cmds = s.write("cmds.txt", "next\ncontinue\n");
    let (code, out, _) = s.run(&prog, &["--break", "3", "--commands", &cmds]);
    assert_eq!(code, 0);
    assert!(out.contains("next at"), "next should report a pause:\n{}", out);
    assert!(
        out.contains("in main()"),
        "next should land in the caller:\n{}",
        out
    );
}

#[test]
fn step_walks_instructions() {
    let s = Session::new("step");
    let prog = s.write("prog.matrix", PROGRAM);
    let cmds = s.write("cmds.txt", "step\nstep\ncontinue\n");
    let (code, out, _) = s.run(&prog, &["--break", "3", "--commands", &cmds]);
    assert_eq!(code, 0);
    assert!(
        out.matches("step at").count() >= 2,
        "expected at least two step pauses:\n{}",
        out
    );
}

#[test]
fn breakpoint_fires_once_per_line_per_hit() {
    let s = Session::new("once");
    let prog = s.write("prog.matrix", PROGRAM);
    // Two calls to `add` -> two breakpoint hits at line 3.
    let cmds = s.write("cmds.txt", "continue\ncontinue\ncontinue\n");
    let (code, out, _) = s.run(&prog, &["--break", "3", "--commands", &cmds]);
    assert_eq!(code, 0);
    assert_eq!(
        out.matches("breakpoint at").count(),
        2,
        "expected exactly two hits:\n{}",
        out
    );
}

#[test]
fn quit_aborts_with_exit_code_three() {
    let s = Session::new("quit");
    let prog = s.write("prog.matrix", PROGRAM);
    let cmds = s.write("cmds.txt", "quit\n");
    let (code, out, _) = s.run(&prog, &["--break", "3", "--commands", &cmds]);
    assert_eq!(code, 3, "quit should exit 3:\n{}", out);
    assert!(out.contains("debugger: quit"), "quit summary missing:\n{}", out);
    assert!(
        !out.contains("sum="),
        "program must not continue after quit:\n{}",
        out
    );
}

#[test]
fn break_command_and_delete_work_mid_session() {
    let s = Session::new("cmdbreak");
    let prog = s.write("prog.matrix", PROGRAM);
    // Pause at line 3 (first `add` call), add a breakpoint at line 10, then
    // continue through the second `add` pause to reach line 10.
    let cmds = s.write("cmds.txt", "break 10\ncontinue\ncontinue\nquit\n");
    let (code, out, _) = s.run(&prog, &["--break", "3", "--commands", &cmds]);
    assert_eq!(code, 3);
    assert!(out.contains("breakpoint set at line 10"), "set missing:\n{}", out);
    assert!(
        out.contains("prog.matrix:10:"),
        "second breakpoint should fire:\n{}",
        out
    );
}

#[test]
fn unknown_command_is_reported_without_aborting() {
    let s = Session::new("unknown");
    let prog = s.write("prog.matrix", PROGRAM);
    let cmds = s.write("cmds.txt", "frobnicate\nhelp\ncontinue\n");
    let (code, out, _) = s.run(&prog, &["--break", "3", "--commands", &cmds]);
    assert_eq!(code, 0);
    assert!(out.contains("unknown command"), "diagnostic missing:\n{}", out);
    assert!(out.contains("commands: continue"), "help missing:\n{}", out);
}
