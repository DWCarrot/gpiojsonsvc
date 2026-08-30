#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Connected,
    Initialized,
    SetInProgress,
    Closed,
}
