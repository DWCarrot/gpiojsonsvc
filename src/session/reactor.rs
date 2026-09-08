use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::future::Future;
use std::os::fd::AsRawFd;
use std::os::fd::RawFd;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

use futures::Sink;
use futures::SinkExt;
use futures::ready;
use pin_project_lite::pin_project;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::gpio::Backend;
use crate::gpio::EdgeEvent;
use crate::gpio::EdgeEventBuffer;
use crate::gpio::EdgeEventType;
use crate::gpio::GPIOError;
use crate::gpio::LineRequest;
use crate::protocol::request::RequestMessage;
use crate::protocol::request::RequestPayload;
use crate::protocol::request::SetRequest;
use crate::protocol::request::TargetConfigRequest;
use crate::protocol::request::TargetSelector;
use crate::protocol::response::EventPayload;
use crate::protocol::response::EventType;
use crate::protocol::response::ResponseMessage;
use crate::system_event::SystemEvent;
use crate::transport::ConnectionError;

use super::command::ReactorCommand;
use super::execute::apply_get_batch;
use super::execute::apply_set_batch;
use super::execute::compile_get_batch;
use super::execute::compile_set_batch;
use super::execute::fold_get_results;
use super::initialized::InitializedSession;
use super::initialized::SessionConfig;
use super::sequence::PendingSetSequence;
use super::state::SessionError;
use super::state::SessionState;

const DEFAULT_COMMAND_CHANNEL_CAPACITY: usize = 32;

/// Raw fd wrapper that does not close on drop; ownership stays with the line request.
#[derive(Debug)]
struct UnownedFd(RawFd);

impl AsRawFd for UnownedFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

pin_project! {
    struct WriteResponseFuture<'a, W>
    where
        W: Sink<ResponseMessage>,
    {
        #[pin]
        writer: Pin<&'a mut W>,
        response: Option<ResponseMessage>,
        sent: bool,
    }
}

impl<'a, W> WriteResponseFuture<'a, W>
where
    W: Sink<ResponseMessage>,
{
    fn new(writer: Pin<&'a mut W>, response: ResponseMessage) -> Self {
        Self {
            writer,
            response: Some(response),
            sent: false,
        }
    }
}

impl<'a, W> Future for WriteResponseFuture<'a, W>
where
    W: Sink<ResponseMessage>,
{
    type Output = Result<(), W::Error>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();
        if !*this.sent {
            ready!(this.writer.as_mut().poll_ready(context))?;
            let response = this
                .response
                .take()
                .expect("response already consumed before completion");
            this.writer.as_mut().start_send(response)?;
            *this.sent = true;
        }
        ready!(this.writer.as_mut().poll_flush(context))?;
        Poll::Ready(Ok(()))
    }
}

/// Transport-facing handle used to forward commands into a session reactor.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    session_id: u64,
    tx: mpsc::Sender<ReactorCommand>,
}

impl SessionHandle {
    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub fn sender(&self) -> &mpsc::Sender<ReactorCommand> {
        &self.tx
    }

    /// Ask the reactor to close without waiting for channel capacity.
    pub fn shutdown(&self) {
        let _ = self.tx.try_send(ReactorCommand::Shutdown);
    }
}

/// Session-local owner of GPIO state and request/reply dispatch.
pub struct SessionReactor<B: Backend, W>
where
    W: Sink<ResponseMessage> + Send + 'static,
    W::Error: ConnectionError,
{
    session_id: u64,
    backend: Arc<B>,
    config: Arc<dyn SessionConfig>,
    state: SessionState,
    writer: Pin<Box<W>>,
    connection_info: String,
    /// Retained so helper futures can re-enter the command stream.
    tx: mpsc::Sender<ReactorCommand>,
    rx: mpsc::Receiver<ReactorCommand>,
    initialized: Option<InitializedSession<B>>,
    pending_set: Option<PendingSetSequence>,
    next_sequence_token: u64,
    gpio_watchers: Vec<JoinHandle<()>>,
}

impl<B, W> SessionReactor<B, W>
where
    B: Backend + 'static,
    W: Sink<ResponseMessage> + Send + 'static,
    W::Error: ConnectionError,
{
    pub fn spawn(
        session_id: u64,
        backend: Arc<B>,
        config: Arc<dyn SessionConfig>,
        writer: W,
        connection_info: String,
    ) -> (SessionHandle, JoinHandle<()>) {
        Self::spawn_with_capacities(
            session_id,
            backend,
            config,
            writer,
            connection_info,
            DEFAULT_COMMAND_CHANNEL_CAPACITY,
            SystemEvent::new(),
        )
    }

    pub fn spawn_with_events(
        session_id: u64,
        backend: Arc<B>,
        config: Arc<dyn SessionConfig>,
        writer: W,
        connection_info: String,
        events: SystemEvent,
    ) -> (SessionHandle, JoinHandle<()>) {
        Self::spawn_with_capacities(
            session_id,
            backend,
            config,
            writer,
            connection_info,
            DEFAULT_COMMAND_CHANNEL_CAPACITY,
            events,
        )
    }

    pub fn spawn_with_capacities(
        session_id: u64,
        backend: Arc<B>,
        config: Arc<dyn SessionConfig>,
        writer: W,
        connection_info: String,
        command_capacity: usize,
        events: SystemEvent,
    ) -> (SessionHandle, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(command_capacity);
        let handle = SessionHandle {
            session_id,
            tx: tx.clone(),
        };
        let reactor = Self {
            session_id,
            backend,
            config,
            state: SessionState::Connected,
            writer: Box::pin(writer),
            connection_info,
            tx,
            rx,
            initialized: None,
            pending_set: None,
            next_sequence_token: 1,
            gpio_watchers: Vec::new(),
        };
        let join = tokio::spawn(reactor.run(events));
        (handle, join)
    }

    pub async fn run(mut self, events: SystemEvent) {
        tracing::info!(
            session_id = self.session_id,
            connection = %self.connection_info,
            "session started"
        );
        let mut seen_seq = 0;
        loop {
            let next_deadline = self
                .pending_set
                .as_ref()
                .and_then(PendingSetSequence::deadline);
            let wake_token = self.pending_set.as_ref().map(PendingSetSequence::token);

            tokio::select! {
                code = events.recv(&mut seen_seq) => {
                    if code == SystemEvent::SHUTDOWN {
                        self.begin_close();
                        break;
                    }
                }
                cmd = self.rx.recv() => {
                    match cmd {
                        Some(cmd) => {
                            if !self.handle_command(cmd).await.unwrap_or(false) {
                                break;
                            }
                        }
                        None => {
                            self.begin_close();
                            break;
                        }
                    }
                }
                _ = sleep_until_optional(next_deadline), if next_deadline.is_some() => {
                    if let Some(token) = wake_token {
                        if self.handle_scheduled_wake(token).await.is_err() {
                            break;
                        }
                    }
                }
            }
        }
        let _ = self.writer.close().await;
        self.state = SessionState::Closed;
        tracing::info!(
            session_id = self.session_id,
            connection = %self.connection_info,
            "session ended"
        );
    }

    async fn handle_command(&mut self, command: ReactorCommand) -> Result<bool, W::Error> {
        match command {
            ReactorCommand::InboundRequest(request) => {
                self.handle_inbound(request).await?;
                Ok(true)
            }
            ReactorCommand::GpioReady { chip_index } => {
                self.handle_gpio_ready(chip_index).await?;
                Ok(true)
            }
            ReactorCommand::ScheduledWake { token } => {
                self.handle_scheduled_wake(token).await?;
                Ok(true)
            }
            ReactorCommand::TransportClosed | ReactorCommand::Shutdown => {
                self.begin_close();
                Ok(false)
            }
        }
    }

    async fn handle_inbound(&mut self, request: RequestMessage) -> Result<(), W::Error> {
        let RequestMessage { id, payload } = request;
        match payload {
            RequestPayload::Init { target } => self.handle_init(id, &target).await,
            RequestPayload::Get { target } => self.handle_get(id, &target).await,
            RequestPayload::Set { target } => self.handle_set(id, &target).await,
        }
    }

    async fn handle_init(
        &mut self,
        request_id: String,
        target: &BTreeMap<String, TargetConfigRequest>,
    ) -> Result<(), W::Error> {
        match self.state {
            SessionState::Connected => {
                match InitializedSession::initialize(
                    request_id.clone(),
                    target,
                    self.backend.as_ref(),
                    self.config.as_ref(),
                ) {
                    Ok(session) => {
                        self.initialized = Some(session);
                        self.state = SessionState::Initialized;
                        self.start_gpio_watchers();
                        self.reply(ResponseMessage::ok(request_id)).await
                    }
                    Err(error) => self.reply(error.into_response(request_id)).await,
                }
            }
            SessionState::Initialized | SessionState::SetSequenceRunning => {
                self.reply(SessionError::AlreadyInitialized.into_response(request_id))
                    .await
            }
            SessionState::Closing | SessionState::Closed => {
                self.reply(SessionError::Closed.into_response(request_id))
                    .await
            }
        }
    }

    async fn handle_get(
        &mut self,
        request_id: String,
        target: &TargetSelector,
    ) -> Result<(), W::Error> {
        match self.require_initialized(&request_id) {
            Ok(()) => {}
            Err(response) => return self.reply(response).await,
        }

        let Some(session) = self.initialized.as_ref() else {
            return self
                .reply(SessionError::NotInitialized.into_response(request_id))
                .await;
        };

        let names = target.as_slice().iter().map(String::as_str);
        let response =
            match compile_get_batch(&session.compiled_targets, names, session.chip_count()) {
                Ok(batch) => match apply_get_batch(session, &batch) {
                    Ok(readings) => {
                        let payload = fold_get_results(target, &readings);
                        ResponseMessage::pin_value(request_id, payload)
                    }
                    Err(error) => error.into_response(request_id),
                },
                Err(error) => error.into_response(request_id),
            };
        self.reply(response).await
    }

    async fn handle_set(
        &mut self,
        request_id: String,
        request: &SetRequest,
    ) -> Result<(), W::Error> {
        match self.require_initialized(&request_id) {
            Ok(()) => {}
            Err(response) => return self.reply(response).await,
        }

        if self.state == SessionState::SetSequenceRunning || self.pending_set.is_some() {
            return self
                .reply(SessionError::SetInProgress.into_response(request_id))
                .await;
        }

        match request {
            SetRequest::Immediate(target) => self.handle_immediate_set(request_id, target).await,
            SetRequest::Steps(steps) => self.handle_set_sequence(request_id, steps).await,
        }
    }

    async fn handle_immediate_set(
        &mut self,
        request_id: String,
        target: &BTreeMap<String, u8>,
    ) -> Result<(), W::Error> {
        let Some(session) = self.initialized.as_ref() else {
            return self
                .reply(SessionError::NotInitialized.into_response(request_id))
                .await;
        };

        let writes = target.iter().map(|(name, value)| (name.as_str(), *value));
        let response =
            match compile_set_batch(&session.compiled_targets, writes, session.chip_count()) {
                Ok(batch) => match apply_set_batch(session, &batch) {
                    Ok(()) => ResponseMessage::ok(request_id),
                    Err(error) => error.into_response(request_id),
                },
                Err(error) => error.into_response(request_id),
            };
        self.reply(response).await
    }

    async fn handle_set_sequence(
        &mut self,
        request_id: String,
        steps: &[crate::protocol::request::SetStepRequest],
    ) -> Result<(), W::Error> {
        let Some(session) = self.initialized.as_ref() else {
            return self
                .reply(SessionError::NotInitialized.into_response(request_id))
                .await;
        };

        let compiled_steps = match PendingSetSequence::compile_steps(
            &session.compiled_targets,
            steps,
            session.chip_count(),
        ) {
            Ok(compiled_steps) => compiled_steps,
            Err(error) => return self.reply(error.into_response(request_id)).await,
        };

        let sequence_started_at = Instant::now();
        let token = self.next_sequence_token;
        self.next_sequence_token = self.next_sequence_token.wrapping_add(1);
        let pending = PendingSetSequence::start(
            token,
            request_id.clone(),
            compiled_steps,
            sequence_started_at,
        );

        if let Err(error) = apply_set_batch(session, pending.first_batch()) {
            return self.reply(error.into_response(request_id)).await;
        }

        if pending.is_complete() {
            return self.reply(ResponseMessage::ok(request_id)).await;
        }

        self.pending_set = Some(pending);
        self.state = SessionState::SetSequenceRunning;
        Ok(())
    }

    async fn handle_scheduled_wake(&mut self, token: u64) -> Result<(), W::Error> {
        let Some(pending) = self.pending_set.as_ref() else {
            return Ok(());
        };
        if !pending.matches_token(token) {
            tracing::debug!(
                session_id = self.session_id,
                token,
                active_token = pending.token(),
                "ignoring stale ScheduledWake"
            );
            return Ok(());
        }

        let request_id = pending.request_id().to_owned();
        let Some(batch) = pending.batch() else {
            self.clear_sequence();
            return self.reply(ResponseMessage::ok(request_id)).await;
        };
        let Some(session) = self.initialized.as_ref() else {
            self.clear_sequence();
            return self
                .reply(SessionError::NotInitialized.into_response(request_id))
                .await;
        };

        if let Err(error) = apply_set_batch(session, batch) {
            tracing::warn!(
                session_id = self.session_id,
                error = %error,
                "failed to apply scheduled set step"
            );
            self.clear_sequence();
            return self.reply(error.into_response(request_id)).await;
        }

        let Some(pending) = self.pending_set.as_mut() else {
            return Ok(());
        };
        if !pending.advance() {
            self.clear_sequence();
            return self.reply(ResponseMessage::ok(request_id)).await;
        }
        Ok(())
    }

    async fn handle_gpio_ready(&mut self, chip_index: u32) -> Result<(), W::Error> {
        let events = {
            let Some(session) = self.initialized.as_mut() else {
                return Ok(());
            };
            let Some(chip) = session
                .chips
                .iter()
                .find(|chip| chip.chip_index == chip_index)
            else {
                return Ok(());
            };

            match chip.request.read_edge_events(&mut session.edge_buffer, 16) {
                Ok(count) => {
                    let mut events = Vec::with_capacity(count);
                    for index in 0..count {
                        let event = match session.edge_buffer.get_event(index) {
                            Ok(event) => event,
                            Err(error) => {
                                tracing::warn!(
                                    session_id = self.session_id,
                                    chip_index,
                                    error = %error,
                                    "failed to read buffered edge event"
                                );
                                continue;
                            }
                        };
                        let offset = event.get_line_offset();
                        let Some(target) = session
                            .compiled_targets
                            .trigger_target_name(chip_index, offset)
                        else {
                            tracing::debug!(
                                session_id = self.session_id,
                                chip_index,
                                offset,
                                "ignoring unmapped trigger pin"
                            );
                            continue;
                        };
                        let kind = match event.get_event_type() {
                            EdgeEventType::RisingEdge => EventType::Rising,
                            EdgeEventType::FallingEdge => EventType::Falling,
                        };
                        events.push(EventPayload {
                            target: target.to_owned(),
                            kind,
                        });
                    }
                    Some((session.init_request_id.clone(), events))
                }
                Err(GPIOError::Other(message)) if message.contains("no edge events") => None,
                Err(error) => {
                    tracing::warn!(
                        session_id = self.session_id,
                        chip_index,
                        error = %error,
                        "failed to drain edge events"
                    );
                    None
                }
            }
        };

        let Some((init_request_id, events)) = events else {
            return Ok(());
        };
        for event in events {
            self.reply(ResponseMessage::event(init_request_id.clone(), event))
                .await?;
        }
        Ok(())
    }

    fn start_gpio_watchers(&mut self) {
        let Some(session) = self.initialized.as_ref() else {
            return;
        };

        let trigger_chips: BTreeSet<u32> = session
            .compiled_targets
            .trigger_by_pin
            .keys()
            .map(|(chip_index, _)| *chip_index)
            .collect();
        if trigger_chips.is_empty() {
            return;
        }

        for chip in &session.chips {
            if !trigger_chips.contains(&chip.chip_index) {
                continue;
            }
            let fd = chip.request.as_raw_fd();
            if fd < 0 {
                tracing::warn!(
                    session_id = self.session_id,
                    chip_index = chip.chip_index,
                    "skipping GPIO watcher for invalid request fd"
                );
                continue;
            }
            let tx = self.tx.clone();
            let chip_index = chip.chip_index;
            let session_id = self.session_id;
            let join = tokio::spawn(async move {
                watch_gpio_fd(session_id, chip_index, fd, tx).await;
            });
            self.gpio_watchers.push(join);
        }
    }

    fn stop_gpio_watchers(&mut self) {
        for handle in self.gpio_watchers.drain(..) {
            handle.abort();
        }
    }

    fn clear_sequence(&mut self) {
        self.pending_set = None;
        if self.state == SessionState::SetSequenceRunning {
            self.state = SessionState::Initialized;
        }
    }

    fn require_initialized(&self, request_id: &str) -> Result<(), ResponseMessage> {
        match self.state {
            SessionState::Initialized | SessionState::SetSequenceRunning => Ok(()),
            SessionState::Connected => Err(SessionError::NotInitialized.into_response(request_id)),
            SessionState::Closing | SessionState::Closed => {
                Err(SessionError::Closed.into_response(request_id))
            }
        }
    }

    async fn reply(&mut self, response: ResponseMessage) -> Result<(), W::Error> {
        WriteResponseFuture::new(self.writer.as_mut(), response).await
    }

    fn begin_close(&mut self) {
        self.state = SessionState::Closing;
        self.clear_sequence();
        self.stop_gpio_watchers();
        self.initialized = None;
    }
}

async fn sleep_until_optional(deadline: Option<Instant>) {
    if let Some(deadline) = deadline {
        tokio::time::sleep_until(deadline).await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn watch_gpio_fd(
    session_id: u64,
    chip_index: u32,
    fd: RawFd,
    tx: mpsc::Sender<ReactorCommand>,
) {
    let async_fd = match AsyncFd::with_interest(UnownedFd(fd), Interest::READABLE) {
        Ok(async_fd) => async_fd,
        Err(error) => {
            tracing::warn!(
                session_id,
                chip_index,
                error = %error,
                "failed to register GPIO request fd for readiness"
            );
            return;
        }
    };

    loop {
        let mut guard = match async_fd.readable().await {
            Ok(guard) => guard,
            Err(error) => {
                tracing::debug!(
                    session_id,
                    chip_index,
                    error = %error,
                    "GPIO readiness watch ended"
                );
                return;
            }
        };
        if tx
            .send(ReactorCommand::GpioReady { chip_index })
            .await
            .is_err()
        {
            return;
        }
        guard.clear_ready();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fmt;
    use std::fs;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::Context;
    use std::task::Poll;
    use std::time::Duration;

    use futures::Sink;
    use tempfile::NamedTempFile;

    use crate::gpio::mock::LineLevel;
    use crate::gpio::mock::MockBackend;
    use crate::gpio::mock::parse_chip_xml;
    use crate::gpio::mock::parse_write_log_blocks;
    use crate::protocol::request::EdgeMode;
    use crate::protocol::request::PinSelector;
    use crate::protocol::request::RequestMessage;
    use crate::protocol::request::RequestPayload;
    use crate::protocol::request::SetRequest;
    use crate::protocol::request::SetStepRequest;
    use crate::protocol::request::TargetConfigRequest;
    use crate::protocol::request::TargetSelector;
    use crate::protocol::response::EventType;
    use crate::protocol::response::PinValuePayload;
    use crate::protocol::response::ResponseMessage;
    use crate::protocol::response::ResponseStatus;
    use crate::transport::ConnectionError;

    use super::ReactorCommand;
    use super::SessionHandle;
    use super::SessionReactor;

    const SAMPLE_XML: &str = r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input" bias="pull_up">L</line>
    <line id="1" name="line1" direction="input" bias="pull_down">H</line>
    <line id="2" name="line2" direction="output" drive="push_pull">L</line>
    <line id="3" name="line3" direction="output" drive="push_pull">H</line>
</gpiochip>"#;

    #[derive(Debug)]
    struct TestSinkError;

    impl fmt::Display for TestSinkError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("test sink closed")
        }
    }

    impl std::error::Error for TestSinkError {}

    impl ConnectionError for TestSinkError {
        fn is_protocol_error(&self) -> bool {
            false
        }

        fn is_transport_error(&self) -> bool {
            true
        }

        fn try_into_io_error(self) -> Result<std::io::Error, Self>
        where
            Self: Sized,
        {
            Err(self)
        }
    }

    #[derive(Debug, Clone)]
    struct TestResponseSink {
        tx: tokio::sync::mpsc::UnboundedSender<ResponseMessage>,
    }

    impl Sink<ResponseMessage> for TestResponseSink {
        type Error = TestSinkError;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, item: ResponseMessage) -> Result<(), Self::Error> {
            self.tx.send(item).map_err(|_error| TestSinkError)
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    fn write_chip_file(contents: &str) -> NamedTempFile {
        let file = NamedTempFile::new().expect("temp file");
        fs::write(file.path(), contents).expect("write chip xml");
        file
    }

    fn sample_init_targets() -> BTreeMap<String, TargetConfigRequest> {
        BTreeMap::from([
            (
                "IN".to_owned(),
                TargetConfigRequest::Input {
                    pin: PinSelector::Single("gpiochip0:0".to_owned()),
                    bias: None,
                },
            ),
            (
                "OUT".to_owned(),
                TargetConfigRequest::Output {
                    pin: PinSelector::Single("gpiochip0:2".to_owned()),
                    drive: None,
                },
            ),
        ])
    }

    fn trigger_init_targets() -> BTreeMap<String, TargetConfigRequest> {
        BTreeMap::from([
            (
                "TRIG".to_owned(),
                TargetConfigRequest::Trigger {
                    pin: "gpiochip0:0".to_owned(),
                    edge: EdgeMode::Both,
                },
            ),
            (
                "OUT".to_owned(),
                TargetConfigRequest::Output {
                    pin: PinSelector::Single("gpiochip0:2".to_owned()),
                    drive: None,
                },
            ),
        ])
    }

    fn spawn_reactor(
        chip_file: &NamedTempFile,
    ) -> (
        SessionHandle,
        tokio::sync::mpsc::UnboundedReceiver<ResponseMessage>,
        tokio::task::JoinHandle<()>,
    ) {
        spawn_reactor_with_backend(chip_file, MockBackend::new())
    }

    fn spawn_reactor_with_backend(
        chip_file: &NamedTempFile,
        backend: MockBackend,
    ) -> (
        SessionHandle,
        tokio::sync::mpsc::UnboundedReceiver<ResponseMessage>,
        tokio::task::JoinHandle<()>,
    ) {
        let path = chip_file.path().to_str().expect("utf8 path").to_owned();
        let config: Arc<dyn super::SessionConfig> = Arc::new(BTreeMap::from([
            (
                "gpiochip0:0".to_owned(),
                crate::config::GPIODPinSpec {
                    device: path.clone(),
                    line: 0,
                },
            ),
            (
                "gpiochip0:2".to_owned(),
                crate::config::GPIODPinSpec {
                    device: path,
                    line: 2,
                },
            ),
        ]));
        let (responses_tx, responses_rx) = tokio::sync::mpsc::unbounded_channel();
        let writer = TestResponseSink { tx: responses_tx };
        let (handle, join) = SessionReactor::spawn(
            1,
            Arc::new(backend),
            config,
            writer,
            "test://reactor".to_owned(),
        );
        (handle, responses_rx, join)
    }

    async fn send_request(handle: &SessionHandle, request: RequestMessage) {
        handle
            .sender()
            .send(ReactorCommand::InboundRequest(request))
            .await
            .expect("send request");
    }

    async fn recv_response(
        responses: &mut tokio::sync::mpsc::UnboundedReceiver<ResponseMessage>,
    ) -> ResponseMessage {
        tokio::time::timeout(Duration::from_secs(2), responses.recv())
            .await
            .expect("response timed out")
            .expect("response channel closed")
    }

    fn line0_xml(level: &str) -> String {
        format!(
            r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input" bias="pull_up">{level}</line>
    <line id="1" name="line1" direction="input" bias="pull_down">H</line>
    <line id="2" name="line2" direction="output" drive="push_pull">L</line>
    <line id="3" name="line3" direction="output" drive="push_pull">H</line>
</gpiochip>"#
        )
    }

    #[tokio::test]
    async fn get_before_init_returns_not_initialized_then_init_succeeds() {
        let chip_file = write_chip_file(SAMPLE_XML);
        let (handle, mut responses, join) = spawn_reactor(&chip_file);

        send_request(
            &handle,
            RequestMessage {
                id: "get-1".to_owned(),
                payload: RequestPayload::Get {
                    target: TargetSelector::Single("IN".to_owned()),
                },
            },
        )
        .await;

        let response = recv_response(&mut responses).await;
        assert_eq!(response.id, "get-1");
        match response.status {
            ResponseStatus::Error { error } => {
                assert!(error.contains("not initialized"));
            }
            other => panic!("expected error, got {other:?}"),
        }

        send_request(
            &handle,
            RequestMessage {
                id: "init-1".to_owned(),
                payload: RequestPayload::Init {
                    target: sample_init_targets(),
                },
            },
        )
        .await;

        let response = recv_response(&mut responses).await;
        assert_eq!(response, ResponseMessage::ok("init-1"));

        handle
            .sender()
            .send(ReactorCommand::TransportClosed)
            .await
            .expect("close");
        join.await.expect("reactor exit");
    }

    #[tokio::test]
    async fn init_get_immediate_set_round_trip() {
        let chip_file = write_chip_file(SAMPLE_XML);
        let (handle, mut responses, join) = spawn_reactor(&chip_file);

        send_request(
            &handle,
            RequestMessage {
                id: "init-1".to_owned(),
                payload: RequestPayload::Init {
                    target: sample_init_targets(),
                },
            },
        )
        .await;
        assert_eq!(
            recv_response(&mut responses).await,
            ResponseMessage::ok("init-1")
        );

        send_request(
            &handle,
            RequestMessage {
                id: "get-1".to_owned(),
                payload: RequestPayload::Get {
                    target: TargetSelector::Single("IN".to_owned()),
                },
            },
        )
        .await;
        let get_response = recv_response(&mut responses).await;
        assert_eq!(
            get_response,
            ResponseMessage::pin_value("get-1", PinValuePayload::Value(0))
        );

        send_request(
            &handle,
            RequestMessage {
                id: "set-1".to_owned(),
                payload: RequestPayload::Set {
                    target: SetRequest::Immediate(BTreeMap::from([("OUT".to_owned(), 1)])),
                },
            },
        )
        .await;
        assert_eq!(
            recv_response(&mut responses).await,
            ResponseMessage::ok("set-1")
        );

        let snapshot = crate::gpio::mock::parse_chip_xml(
            &fs::read_to_string(chip_file.path()).expect("read chip xml"),
        )
        .expect("parse chip xml");
        assert_eq!(
            snapshot.lines.get(&2).expect("line 2").persisted_level,
            crate::gpio::mock::LineLevel::High
        );

        handle
            .sender()
            .send(ReactorCommand::TransportClosed)
            .await
            .expect("close");
        join.await.expect("reactor exit");
    }

    #[tokio::test]
    async fn second_init_is_rejected() {
        let chip_file = write_chip_file(SAMPLE_XML);
        let (handle, mut responses, join) = spawn_reactor(&chip_file);

        send_request(
            &handle,
            RequestMessage {
                id: "init-1".to_owned(),
                payload: RequestPayload::Init {
                    target: sample_init_targets(),
                },
            },
        )
        .await;
        assert_eq!(
            recv_response(&mut responses).await,
            ResponseMessage::ok("init-1")
        );

        send_request(
            &handle,
            RequestMessage {
                id: "init-2".to_owned(),
                payload: RequestPayload::Init {
                    target: sample_init_targets(),
                },
            },
        )
        .await;
        let response = recv_response(&mut responses).await;
        assert_eq!(response.id, "init-2");
        match response.status {
            ResponseStatus::Error { error } => {
                assert!(error.contains("already initialized"));
            }
            other => panic!("expected already-initialized error, got {other:?}"),
        }

        send_request(
            &handle,
            RequestMessage {
                id: "get-1".to_owned(),
                payload: RequestPayload::Get {
                    target: TargetSelector::Single("IN".to_owned()),
                },
            },
        )
        .await;
        assert_eq!(
            recv_response(&mut responses).await,
            ResponseMessage::pin_value("get-1", PinValuePayload::Value(0))
        );

        handle
            .sender()
            .send(ReactorCommand::TransportClosed)
            .await
            .expect("close");
        join.await.expect("reactor exit");
    }

    #[tokio::test]
    async fn set_with_steps_runs_to_completion() {
        let chip_file = write_chip_file(SAMPLE_XML);
        let log_file = NamedTempFile::new().expect("temp write log");
        let backend = MockBackend::new()
            .with_write_log(log_file.path())
            .expect("open write log");
        let write_log = backend.write_log().expect("write log enabled");
        let (handle, mut responses, join) = spawn_reactor_with_backend(&chip_file, backend);

        send_request(
            &handle,
            RequestMessage {
                id: "init-1".to_owned(),
                payload: RequestPayload::Init {
                    target: sample_init_targets(),
                },
            },
        )
        .await;
        assert_eq!(
            recv_response(&mut responses).await,
            ResponseMessage::ok("init-1")
        );

        send_request(
            &handle,
            RequestMessage {
                id: "set-steps".to_owned(),
                payload: RequestPayload::Set {
                    target: SetRequest::Steps(vec![
                        SetStepRequest {
                            lag: 0,
                            target: BTreeMap::from([("OUT".to_owned(), 1)]),
                        },
                        SetStepRequest {
                            lag: 30,
                            target: BTreeMap::from([("OUT".to_owned(), 0)]),
                        },
                        SetStepRequest {
                            lag: 30,
                            target: BTreeMap::from([("OUT".to_owned(), 1)]),
                        },
                    ]),
                },
            },
        )
        .await;
        assert_eq!(
            recv_response(&mut responses).await,
            ResponseMessage::ok("set-steps")
        );

        write_log.flush();
        let content = fs::read_to_string(log_file.path()).expect("read write log");
        let blocks = parse_write_log_blocks(&content);

        assert_eq!(blocks.len(), 3, "one dump per set step");
        assert!(
            blocks[0].0 <= blocks[1].0 && blocks[1].0 <= blocks[2].0,
            "timestamps must be non-decreasing"
        );

        let expected = [LineLevel::High, LineLevel::Low, LineLevel::High];
        for (index, ((_, xml), expected_level)) in blocks.iter().zip(expected).enumerate() {
            let snapshot = parse_chip_xml(xml).expect("parse write-log dump");
            assert_eq!(
                snapshot.lines.get(&2).expect("line 2").persisted_level,
                expected_level,
                "step {index} OUT level"
            );
        }

        handle
            .sender()
            .send(ReactorCommand::TransportClosed)
            .await
            .expect("close");
        join.await.expect("reactor exit");
    }

    #[tokio::test]
    async fn set_while_sequence_running_is_rejected() {
        let chip_file = write_chip_file(SAMPLE_XML);
        let (handle, mut responses, join) = spawn_reactor(&chip_file);

        send_request(
            &handle,
            RequestMessage {
                id: "init-1".to_owned(),
                payload: RequestPayload::Init {
                    target: sample_init_targets(),
                },
            },
        )
        .await;
        assert_eq!(
            recv_response(&mut responses).await,
            ResponseMessage::ok("init-1")
        );

        send_request(
            &handle,
            RequestMessage {
                id: "set-steps".to_owned(),
                payload: RequestPayload::Set {
                    target: SetRequest::Steps(vec![
                        SetStepRequest {
                            lag: 0,
                            target: BTreeMap::from([("OUT".to_owned(), 1)]),
                        },
                        SetStepRequest {
                            lag: 200,
                            target: BTreeMap::from([("OUT".to_owned(), 0)]),
                        },
                    ]),
                },
            },
        )
        .await;

        send_request(
            &handle,
            RequestMessage {
                id: "set-2".to_owned(),
                payload: RequestPayload::Set {
                    target: SetRequest::Immediate(BTreeMap::from([("OUT".to_owned(), 0)])),
                },
            },
        )
        .await;
        let response = recv_response(&mut responses).await;
        assert_eq!(response.id, "set-2");
        match response.status {
            ResponseStatus::Error { error } => {
                assert!(error.contains("already in progress"));
            }
            other => panic!("expected set-in-progress error, got {other:?}"),
        }
        assert_eq!(
            recv_response(&mut responses).await,
            ResponseMessage::ok("set-steps")
        );

        handle
            .sender()
            .send(ReactorCommand::TransportClosed)
            .await
            .expect("close");
        join.await.expect("reactor exit");
    }

    #[tokio::test]
    async fn trigger_edge_emits_event_with_init_request_id() {
        let chip_file = write_chip_file(SAMPLE_XML);
        let backend = MockBackend::with_poll_interval(Duration::from_millis(20));
        let (handle, mut responses, join) = spawn_reactor_with_backend(&chip_file, backend);

        send_request(
            &handle,
            RequestMessage {
                id: "init-trig".to_owned(),
                payload: RequestPayload::Init {
                    target: trigger_init_targets(),
                },
            },
        )
        .await;
        assert_eq!(
            recv_response(&mut responses).await,
            ResponseMessage::ok("init-trig")
        );

        fs::write(chip_file.path(), line0_xml("H")).expect("external rising edit");

        let event = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let response = recv_response(&mut responses).await;
                match response.status {
                    ResponseStatus::Event { event } => {
                        assert_eq!(response.id, "init-trig");
                        return event;
                    }
                    other => panic!("unexpected response while waiting for event: {other:?}"),
                }
            }
        })
        .await
        .expect("timed out waiting for trigger event");

        assert_eq!(event.target, "TRIG");
        assert_eq!(event.kind, EventType::Rising);

        handle
            .sender()
            .send(ReactorCommand::TransportClosed)
            .await
            .expect("close");
        join.await.expect("reactor exit");
    }

    #[tokio::test]
    async fn transport_closed_ends_the_reactor_task() {
        let chip_file = write_chip_file(SAMPLE_XML);
        let (handle, mut responses, join) = spawn_reactor(&chip_file);

        send_request(
            &handle,
            RequestMessage {
                id: "init-1".to_owned(),
                payload: RequestPayload::Init {
                    target: sample_init_targets(),
                },
            },
        )
        .await;
        assert_eq!(
            recv_response(&mut responses).await,
            ResponseMessage::ok("init-1")
        );

        handle
            .sender()
            .send(ReactorCommand::TransportClosed)
            .await
            .expect("close");

        tokio::time::timeout(Duration::from_secs(2), join)
            .await
            .expect("reactor did not exit")
            .expect("reactor join");
    }

    #[tokio::test]
    async fn shutdown_event_ends_the_reactor_task() {
        let chip_file = write_chip_file(SAMPLE_XML);
        let path = chip_file.path().to_str().expect("utf8 path").to_owned();
        let config: Arc<dyn super::SessionConfig> = Arc::new(BTreeMap::from([
            (
                "gpiochip0:0".to_owned(),
                crate::config::GPIODPinSpec {
                    device: path.clone(),
                    line: 0,
                },
            ),
            (
                "gpiochip0:2".to_owned(),
                crate::config::GPIODPinSpec {
                    device: path,
                    line: 2,
                },
            ),
        ]));
        let (responses_tx, _responses_rx) = tokio::sync::mpsc::unbounded_channel();
        let writer = TestResponseSink { tx: responses_tx };
        let events = crate::system_event::SystemEvent::new();
        let (_handle, join) = SessionReactor::spawn_with_capacities(
            1,
            Arc::new(MockBackend::new()),
            config,
            writer,
            "test://reactor".to_owned(),
            32,
            events.clone(),
        );

        events.emit(crate::system_event::SystemEvent::SHUTDOWN);
        tokio::time::timeout(Duration::from_secs(2), join)
            .await
            .expect("reactor did not exit")
            .expect("reactor join");
    }

    #[tokio::test]
    async fn deferred_commands_do_not_panic_or_emit_responses() {
        let chip_file = write_chip_file(SAMPLE_XML);
        let (handle, mut responses, join) = spawn_reactor(&chip_file);

        handle
            .sender()
            .send(ReactorCommand::GpioReady { chip_index: 0 })
            .await
            .expect("gpio ready");
        handle
            .sender()
            .send(ReactorCommand::ScheduledWake { token: 1 })
            .await
            .expect("scheduled wake");

        send_request(
            &handle,
            RequestMessage {
                id: "init-1".to_owned(),
                payload: RequestPayload::Init {
                    target: sample_init_targets(),
                },
            },
        )
        .await;
        assert_eq!(
            recv_response(&mut responses).await,
            ResponseMessage::ok("init-1")
        );

        // No unexpected responses should have been queued ahead of init.
        assert!(responses.try_recv().is_err());

        handle
            .sender()
            .send(ReactorCommand::Shutdown)
            .await
            .expect("shutdown");
        join.await.expect("reactor exit");
    }
}
