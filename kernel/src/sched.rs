//! Cooperative/pre-emptive kernel thread run-queue (`R13.5`).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadState {
    Empty,
    Ready,
    Running,
    Blocked,
    Exited,
}

#[derive(Clone, Copy, Debug)]
pub struct ThreadControlBlock {
    pub tid: u32,
    pub state: ThreadState,
    pub stack_top: u64,
    pub ticks: u64,
}

pub struct RunQueue<const N: usize> {
    slots: [ThreadControlBlock; N],
    current: usize,
}

impl<const N: usize> RunQueue<N> {
    pub const fn new() -> Self {
        Self {
            slots: [ThreadControlBlock {
                tid: 0,
                state: ThreadState::Empty,
                stack_top: 0,
                ticks: 0,
            }; N],
            current: 0,
        }
    }

    pub fn spawn(&mut self, tid: u32, stack_top: u64) -> Result<usize, &'static str> {
        for (i, s) in self.slots.iter_mut().enumerate() {
            if s.state == ThreadState::Empty || s.state == ThreadState::Exited {
                *s = ThreadControlBlock {
                    tid,
                    state: ThreadState::Ready,
                    stack_top,
                    ticks: 0,
                };
                return Ok(i);
            }
        }
        Err("sched: run queue full")
    }

    /// Round-robin tick: advance to next `Ready` thread.
    pub fn schedule_next(&mut self) -> Option<u32> {
        if self.slots[self.current].state == ThreadState::Running {
            self.slots[self.current].state = ThreadState::Ready;
        }
        for offset in 1..=N {
            let idx = (self.current + offset) % N;
            if self.slots[idx].state == ThreadState::Ready {
                self.slots[idx].state = ThreadState::Running;
                self.slots[idx].ticks += 1;
                self.current = idx;
                return Some(self.slots[idx].tid);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_robin_ping_pong_is_fair() {
        let mut rq = RunQueue::<4>::new();
        rq.spawn(1, 0x1000).unwrap();
        rq.spawn(2, 0x2000).unwrap();
        assert_eq!(rq.schedule_next(), Some(2));
        assert_eq!(rq.schedule_next(), Some(1));
        assert_eq!(rq.schedule_next(), Some(2));
        assert_eq!(rq.schedule_next(), Some(1));
    }
}
