//! The 2026-10-22 I/O additions: task cancellation (`task.cancel()`) and
//! readiness polling (`tcp_poll`). Both engines must behave identically.

use spid3r_lang::diagnostics::Diagnostic;
use spid3r_lang::interpreter::Interp;
use spid3r_lang::modules;
use spid3r_lang::parser;
use spid3r_lang::semantic;
use spid3r_lang::vm::Vm;
use std::sync::Arc;

fn run_tree(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("io_add.matrix"))?;
    semantic::check_file(&program, true, "io_add.matrix")?;
    let interp = Interp::new(Arc::from("io_add.matrix"), modules::default_module_dirs());
    interp.run_program(&program)
}

fn run_vm(src: &str) -> Result<i32, Diagnostic> {
    let program = parser::parse_file(src, Arc::from("io_add.matrix"))?;
    semantic::check_file(&program, true, "io_add.matrix")?;
    let dirs = modules::default_module_dirs();
    let mut vm = Vm::new(Arc::from("io_add.matrix"), dirs);
    vm.run_program(&program)
}

const CANCEL_CODE: &str = r#"
fn main() {
    // 1) a runaway compute task stops at its cancellation point
    let spin = spawn fn() {
        let i = 0;
        while true { i = i + 1; }
    };
    sleep_ms(80);
    assert_eq(spin.cancel(), true);
    sleep_ms(120);
    assert_eq(spin.status(), "cancelled");
    let raised = false;
    try {
        spin.join();
    } catch (e) {
        raised = true;
    }
    assert_eq(raised, true);

    // 2) a task blocked on an empty channel stops too
    let ch = channel();
    let waiter = spawn fn() {
        ch.recv();
        return 1;
    };
    sleep_ms(80);
    assert_eq(waiter.cancel(), true);
    sleep_ms(250);
    assert_eq(waiter.status(), "cancelled");

    // 3) cancelling a finished task is a no-op
    let done = spawn fn() { return 42; };
    assert_eq(done.join(), 42);
    assert_eq(done.cancel(), false);
    assert_eq(done.status(), "done");
    return 0;
}
"#;

#[test]
fn task_cancellation_works_on_both_engines() {
    assert_eq!(run_tree(CANCEL_CODE).unwrap(), 0);
    assert_eq!(run_vm(CANCEL_CODE).unwrap(), 0);
}

const POLL_CODE: &str = r#"
fn main() {
    let listener = tcp_listen("127.0.0.1:0").unwrap();
    spawn fn() {
        let conn = tcp_accept(listener).unwrap();
        sleep_ms(80);
        tcp_write(conn, "hello");
    };
    sleep_ms(40);
    let client = tcp_connect(tcp_local_addr(listener).unwrap()).unwrap();
    // Nothing to read yet: the poll times out with an empty readable list.
    let before = tcp_poll([client], 40).unwrap();
    assert_eq(len(before["readable"]), 0);
    // Data arrives: the connection shows up as readable.
    let after = tcp_poll([client], 3000).unwrap();
    assert_eq(len(after["readable"]), 1);
    // The bytes the probe read ahead are preserved, not lost.
    assert_eq(tcp_read(client, 64).unwrap(), "hello");
    tcp_close(listener);
    tcp_close(client);
    return 0;
}
"#;

#[test]
fn tcp_poll_reports_readiness_and_preserves_data_on_both_engines() {
    assert_eq!(run_tree(POLL_CODE).unwrap(), 0);
    assert_eq!(run_vm(POLL_CODE).unwrap(), 0);
}

#[test]
fn tcp_poll_rejects_a_non_list_argument() {
    let code = r#"
fn main() {
    tcp_poll(7, 10);
    return 0;
}
"#;
    assert!(run_tree(code).is_err());
    assert!(run_vm(code).is_err());
}

const WS_CODE: &str = r#"
fn main() {
    let listener = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(listener).unwrap();
    // Server: accept, upgrade to a WebSocket, echo one frame, close.
    spawn fn() {
        let conn = tcp_accept(listener).unwrap();
        let ws = ws_accept(conn).unwrap();
        let msg = ws_recv(ws).unwrap();
        ws_send(ws, "echo:" + msg);
        sleep_ms(200);
        ws_close(ws);
    };
    let ws = ws_connect("ws://" + addr).unwrap();
    ws_send(ws, "hello");
    assert_eq(ws_recv(ws).unwrap(), "echo:hello");
    ws_close(ws);
    return 0;
}
"#;

#[test]
fn websocket_handshake_and_frames_on_both_engines() {
    assert_eq!(run_tree(WS_CODE).unwrap(), 0);
    assert_eq!(run_vm(WS_CODE).unwrap(), 0);
}

#[test]
fn websocket_rejects_a_non_ws_url() {
    let code = r#"
fn main() {
    ws_connect("https://example.com");
    return 0;
}
"#;
    assert!(run_tree(code).is_err());
    assert!(run_vm(code).is_err());
}

const MULTICAST_CODE: &str = r#"
fn main() {
    let a = udp_bind("0.0.0.0:0").unwrap();
    let b = udp_bind("0.0.0.0:0").unwrap();
    let group = "239.10.20.30";
    udp_join_multicast(a, group, "0.0.0.0").unwrap();
    udp_join_multicast(b, group, "0.0.0.0").unwrap();
    udp_set_multicast_ttl(a, 1).unwrap();
    udp_set_broadcast(a, true).unwrap();
    // Send to the group address at b's port: b receives it.
    let port = udp_local_addr(b).unwrap().split(":")[1];
    udp_send_to(a, group + ":" + port, "multicast-hello").unwrap();
    let got = udp_recv_from(b, 128).unwrap();
    assert_eq(got[0], "multicast-hello");
    udp_leave_multicast(b, group, "0.0.0.0").unwrap();
    return 0;
}
"#;

#[test]
fn udp_multicast_delivers_on_both_engines() {
    assert_eq!(run_tree(MULTICAST_CODE).unwrap(), 0);
    assert_eq!(run_vm(MULTICAST_CODE).unwrap(), 0);
}

const EVENT_LOOP_CODE: &str = r#"
import "event" as ev;

fn main() {
    let listener = tcp_listen("127.0.0.1:0").unwrap();
    let addr = tcp_local_addr(listener).unwrap();
    let c1 = tcp_connect(addr).unwrap();
    let c2 = tcp_connect(addr).unwrap();
    let a1 = tcp_accept(listener).unwrap();
    let a2 = tcp_accept(listener).unwrap();
    tcp_write(c1, "one");
    tcp_write(c2, "two");
    let el = ev.start();
    ev.on_readable(el, a1, fn(c) {
        let data = tcp_read(c, 64).unwrap();
        tcp_write(c, "A:" + data);
        ev.remove(el, c);
    });
    ev.on_readable(el, a2, fn(c) {
        let data = tcp_read(c, 64).unwrap();
        tcp_write(c, "B:" + data);
        ev.remove(el, c);
    });
    // The loop returns once every watcher is removed.
    ev.run(el, 1000);
    assert_eq(tcp_read(c1, 64).unwrap(), "A:one");
    assert_eq(tcp_read(c2, 64).unwrap(), "B:two");
    tcp_close(listener);
    tcp_close(c1);
    tcp_close(c2);
    return 0;
}
"#;

#[test]
fn event_loop_multiplexes_connections_on_both_engines() {
    assert_eq!(run_tree(EVENT_LOOP_CODE).unwrap(), 0);
    assert_eq!(run_vm(EVENT_LOOP_CODE).unwrap(), 0);
}
