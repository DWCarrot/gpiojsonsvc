use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use super::state::SharedChipState;
use super::state::apply_external_file_diff;

/// Background polling watcher for one opened mock chip XML file.
#[derive(Debug)]
pub struct ChipWatcher {
    state: SharedChipState,
    poll_interval: Duration,
    started: AtomicBool,
    ready: AtomicBool,
}

impl ChipWatcher {
    pub fn new(state: SharedChipState, poll_interval: Duration) -> Self {
        Self {
            state,
            poll_interval: poll_interval.max(Duration::from_millis(1)),
            started: AtomicBool::new(false),
            ready: AtomicBool::new(false),
        }
    }

    pub fn wait_until_ready(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.ready.load(Ordering::Acquire) {
                return true;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        self.ready.load(Ordering::Acquire)
    }

    pub fn start_if_needed(self: &Arc<Self>) {
        if self
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let shared = Arc::clone(self);
        if let Err(error) = thread::Builder::new()
            .name("mock-gpio-poller".to_owned())
            .spawn({
                let watcher = Arc::clone(&shared);
                move || Self::watch_loop(watcher)
            })
        {
            shared.set_watcher_error(format!("failed to spawn watcher thread: {error}"));
        }
    }

    fn set_watcher_error(&self, message: String) {
        if let Ok(mut state) = self.state.lock() {
            if state.watcher_error.is_none() {
                state.watcher_error = Some(message);
            }
        }
    }

    fn watch_loop(self_arc: Arc<Self>) {
        self_arc.ready.store(true, Ordering::Release);
        let mut previous_content = String::new();
        loop {
            if Arc::strong_count(&self_arc) <= 1 {
                break;
            }
            thread::sleep(self_arc.poll_interval);
            if Arc::strong_count(&self_arc) <= 1 {
                break;
            }
            self_arc.reload_from_disk(&mut previous_content);
        }
    }

    fn reload_from_disk(&self, previous_content: &mut String) {
        let xml_path = match self.state.lock() {
            Ok(state) => {
                if state.watcher_error.is_some() {
                    return;
                }
                state.xml_path.clone()
            }
            Err(_) => return,
        };

        let content = match fs::read_to_string(&xml_path) {
            Ok(content) => content,
            Err(error) => {
                self.set_watcher_error(format!("failed to reload {}: {error}", xml_path.display()));
                return;
            }
        };
        if &content == previous_content {
            return;
        }
        *previous_content = content.clone();

        if let Ok(mut state) = self.state.lock() {
            if state.watcher_error.is_some() {
                return;
            }
            apply_external_file_diff(&mut state, &content);
        }
    }
}
