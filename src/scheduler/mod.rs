#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleState {
    Idle,
    Running,
    Cancelled,
    Completed,
}
