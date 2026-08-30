use crate::protocol::request::RequestMessage;

/// Commands delivered to a session reactor through its inbound channel.
#[derive(Debug)]
pub enum ReactorCommand {
    InboundRequest(RequestMessage),
    GpioReady {
        chip_index: u32,
    },
    /// Wake a pending multi-step `set`. Stale tokens are ignored.
    ScheduledWake {
        token: u64,
    },
    TransportClosed,
    Shutdown,
}
