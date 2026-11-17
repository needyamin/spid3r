//! Capability-free monotonic timers (`after_ms`, `every_ms`, `timer_cancel`,
//! `timer_pending`, `timer_check`).
//!
//! Timers use [`crate::sched::now_ms`] (the single process-wide monotonic
//! millisecond clock) and a min-heap (`BinaryHeap<Reverse<(deadline_ms, id)>>`).
//! There is no background timer thread: timers fire when [`timer_check()`] (or
//! the event loop) polls expired deadlines.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::sched;
use crate::value::collections;
use crate::value::{NativeCtx, Value};

use super::{want_int, NativeDef};

#[derive(Debug, Clone)]
struct TimerEntry {
    deadline_ms: i64,
    interval_ms: Option<i64>,
}

#[derive(Default)]
struct TimerState {
    entries: HashMap<i64, TimerEntry>,
    heap: BinaryHeap<Reverse<(i64, i64)>>,
}

static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

fn state() -> &'static Mutex<TimerState> {
    static STATE: OnceLock<Mutex<TimerState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(TimerState::default()))
}

fn schedule(delay_ms: i64, interval_ms: Option<i64>) -> i64 {
    let id = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    let deadline_ms = sched::now_ms().saturating_add(delay_ms);
    let mut g = state().lock().unwrap();
    g.entries.insert(
        id,
        TimerEntry {
            deadline_ms,
            interval_ms,
        },
    );
    g.heap.push(Reverse((deadline_ms, id)));
    id
}

pub fn is_pending(id: i64) -> bool {
    state().lock().unwrap().entries.contains_key(&id)
}

/// Earliest active timer deadline in `sched::now_ms()` units, if any.
/// Lazily discards stale/cancelled heap entries.
pub fn next_deadline_ms() -> Option<i64> {
    let mut g = state().lock().unwrap();
    while let Some(&Reverse((dl, id))) = g.heap.peek() {
        match g.entries.get(&id) {
            Some(e) if e.deadline_ms == dl => return Some(dl),
            _ => {
                g.heap.pop();
            }
        }
    }
    None
}

/// Pop and return all timer IDs whose deadline is `<= now`, in `(deadline, id)`
/// order. One-shot timers are removed; interval timers are rescheduled for
/// `now + period_ms`.
pub fn pop_expired(now: i64) -> Vec<i64> {
    let mut fired = Vec::new();
    let mut g = state().lock().unwrap();
    while let Some(&Reverse((dl, id))) = g.heap.peek() {
        if dl > now {
            break;
        }
        g.heap.pop();
        let Some(entry) = g.entries.get(&id).cloned() else {
            continue; // cancelled
        };
        if entry.deadline_ms != dl {
            continue; // stale heap entry
        }
        fired.push(id);
        if let Some(period) = entry.interval_ms {
            let next_dl = now.saturating_add(period);
            if let Some(e) = g.entries.get_mut(&id) {
                e.deadline_ms = next_dl;
            }
            g.heap.push(Reverse((next_dl, id)));
        } else {
            g.entries.remove(&id);
        }
    }
    fired
}

pub fn native_defs() -> Vec<NativeDef> {
    vec![
        ("after_ms", n_after_ms),
        ("every_ms", n_every_ms),
        ("timer_cancel", n_timer_cancel),
        ("timer_pending", n_timer_pending),
        ("timer_check", n_timer_check),
    ]
}

fn n_after_ms(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    collections::want("after_ms", &args, 1)?;
    let ms = want_int(&args[0], "after_ms")?;
    if ms < 0 {
        return Err("after_ms: delay must not be negative".to_string());
    }
    Ok(Value::Int(schedule(ms, None)))
}

fn n_every_ms(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    collections::want("every_ms", &args, 1)?;
    let ms = want_int(&args[0], "every_ms")?;
    if ms <= 0 {
        return Err("every_ms: interval must be positive".to_string());
    }
    Ok(Value::Int(schedule(ms, Some(ms))))
}

fn n_timer_cancel(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    collections::want("timer_cancel", &args, 1)?;
    let id = want_int(&args[0], "timer_cancel")?;
    let removed = state().lock().unwrap().entries.remove(&id).is_some();
    Ok(Value::Bool(removed))
}

fn n_timer_pending(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    collections::want("timer_pending", &args, 1)?;
    let id = want_int(&args[0], "timer_pending")?;
    let pending = state().lock().unwrap().entries.contains_key(&id);
    Ok(Value::Bool(pending))
}

fn n_timer_check(_ctx: &NativeCtx, args: Vec<Value>) -> Result<Value, String> {
    collections::want("timer_check", &args, 0)?;
    let fired = pop_expired(sched::now_ms());
    let vals: Vec<Value> = fired.into_iter().map(Value::Int).collect();
    collections::construct("list", vals)
}
