use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use tokio::sync::Notify;
use tokio::time::Instant;
use tokio::time::sleep_until;

const ENABLED_MASK: usize = 1;
const GENERATION_INC: usize = 1 << 1;

pub struct ScheduleControl {
    state: AtomicUsize,   // bit0: enabled, bits1..: generation
    notify: Notify,
}

impl ScheduleControl {

    pub fn new() -> Self {
        Self {
            state: AtomicUsize::new(0), // bit 0: enabled, bits 1..: generation
            notify: Notify::new(),
        }
    }

    pub fn enable(&self) {
        let mut old = self.state.load(Ordering::Acquire);
        loop {
            if old & ENABLED_MASK != 0 {
                break; // already enabled
            }
            let new = old | ENABLED_MASK; // only set enabled bit, generation unchanged
            match self.state.compare_exchange_weak(old, new, Ordering::Release, Ordering::Acquire) {
                Ok(_) => break,
                Err(x) => old = x,
            }
        }
    }

    pub fn disable(&self) {
        let mut old = self.state.load(Ordering::Acquire);
        let mut succeeded = false;
        loop {
            if old & ENABLED_MASK == 0 {
                break; // already disabled
            }
            // clear enabled bit, and increment generation
            let new = (old & !ENABLED_MASK).wrapping_add(GENERATION_INC);
            match self.state.compare_exchange_weak(old, new, Ordering::Release, Ordering::Acquire) {
                Ok(_) => {
                    succeeded = true;
                    break;
                }
                Err(x) => old = x,
            }
        }
        if succeeded {
            self.notify.notify_waiters();
        }
    }

    /// Wait for the the target time to be reached.
    /// Return true if the target time is reached, false if the schedule is disabled.
    pub async fn wait(&self, target: Instant) -> bool {
        loop {
            let state = self.state.load(Ordering::Acquire);
            let enabled = state & ENABLED_MASK != 0;
            if !enabled {
                return false;
            }
            let cur_gen = state >> 1;   // current generation

            let notified = self.notify.notified();

            // double check: read state again, check if disabled
            let state2 = self.state.load(Ordering::Acquire);
            let enabled2 = state2 & ENABLED_MASK != 0;
            let gen2 = state2 >> 1;
            if !enabled2 || gen2 != cur_gen {
                continue;   // state changed during create notified, retry
            }

            let sleep = sleep_until(target);
            tokio::select! {
                _ = sleep => return true,   // normal timeout
                _ = notified => continue,     // woke up, maybe disabled, check state again
            }
        }
    }
}