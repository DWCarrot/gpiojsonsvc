//! Process-wide event broadcast: a [`Notify`] plus one [`AtomicU64`] of packed state.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use tokio::sync::Notify;

/// Broadcasts numeric system events to all waiters.
///
/// The atomic word stores `(sequence << 32) | code`. Sequence increments on
/// every [`emit`](Self::emit) so waiters can observe more than one event
/// without spinning on a sticky code. `0` is reserved (no event).
#[derive(Debug, Clone)]
pub struct SystemEvent {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    notify: Notify,
    state: AtomicU64,
}

impl SystemEvent {
    /// No event has been posted yet.
    pub const NONE: u64 = 0;
    /// Process shutdown. All sessions should close.
    pub const SHUTDOWN: u64 = 1;

    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                notify: Notify::new(),
                state: AtomicU64::new(0),
            }),
        }
    }

    /// Post `code` and wake every waiter. `code` must be non-zero; only the
    /// low 32 bits are stored.
    pub fn emit(&self, code: u64) {
        assert!(code != Self::NONE, "system event code 0 is reserved");
        let code = code & 0xFFFF_FFFF;
        let mut prev = self.inner.state.load(Ordering::Relaxed);
        loop {
            let seq = prev >> 32;
            let next = (seq.wrapping_add(1) << 32) | code;
            match self.inner.state.compare_exchange_weak(
                prev,
                next,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => prev = actual,
            }
        }
        self.inner.notify.notify_waiters();
    }

    /// Latest posted code, or [`NONE`](Self::NONE).
    pub fn current(&self) -> u64 {
        self.inner.state.load(Ordering::Acquire) & 0xFFFF_FFFF
    }

    /// Wait until an event newer than `seen_seq` is posted, then return its code.
    ///
    /// `seen_seq` starts at `0`. After each return it holds the sequence that
    /// produced the code, so the next call waits for a later `emit`.
    pub async fn recv(&self, seen_seq: &mut u32) -> u64 {
        loop {
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let state = self.inner.state.load(Ordering::Acquire);
            let seq = (state >> 32) as u32;
            let code = state & 0xFFFF_FFFF;
            if seq != *seen_seq && code != Self::NONE {
                *seen_seq = seq;
                return code;
            }
            notified.await;
        }
    }
}

impl Default for SystemEvent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::SystemEvent;

    #[tokio::test]
    async fn recv_returns_emitted_code() {
        let events = SystemEvent::new();
        let wait = events.clone();
        let join = tokio::spawn(async move {
            let mut seen = 0;
            wait.recv(&mut seen).await
        });

        events.emit(SystemEvent::SHUTDOWN);
        let code = tokio::time::timeout(Duration::from_secs(2), join)
            .await
            .expect("recv timed out")
            .expect("join");
        assert_eq!(code, SystemEvent::SHUTDOWN);
        assert_eq!(events.current(), SystemEvent::SHUTDOWN);
    }

    #[tokio::test]
    async fn recv_is_sticky_if_emit_happens_first() {
        let events = SystemEvent::new();
        events.emit(SystemEvent::SHUTDOWN);
        let mut seen = 0;
        let code = tokio::time::timeout(Duration::from_secs(2), events.recv(&mut seen))
            .await
            .expect("recv timed out");
        assert_eq!(code, SystemEvent::SHUTDOWN);
        assert_eq!(seen, 1);
    }

    #[tokio::test]
    async fn recv_delivers_a_second_distinct_code() {
        let events = SystemEvent::new();
        const RELOAD: u64 = 2;
        let mut seen = 0;
        events.emit(SystemEvent::SHUTDOWN);
        assert_eq!(events.recv(&mut seen).await, SystemEvent::SHUTDOWN);
        events.emit(RELOAD);
        assert_eq!(events.recv(&mut seen).await, RELOAD);
        assert_eq!(events.current(), RELOAD);
    }
}
