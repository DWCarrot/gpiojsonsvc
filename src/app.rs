use crate::config::ServiceConfig;
use crate::error::AppError;
use crate::gpio::Backend;
use crate::gpio::mock::MockBackend;
use crate::gpio::mock::MockChipSnapshot;
use crate::session::SessionConfig;
use crate::session::SessionHandle;
use crate::session::SessionReactor;
use crate::system_event::SystemEvent;
use crate::transport::Connection;
use crate::transport::ServerConfig;
use crate::transport::spawn_reader_task;
use crate::transport::uds::UdsConnection;

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::net::UnixListener;
use tokio::net::UnixStream;
use tokio::task::JoinHandle;
use tokio::time::Instant;

/// Environment variable that enables the mock backend write log.
pub const MOCK_LOG_ENV_VAR: &str = "GPIOJSONSVC_MOCK_LOG";

pub const HELP: &str = "\
gpiojsonsvc [OPTIONS] [CONFIG]

GPIO JSON service over a Unix domain socket.

Arguments:
  [CONFIG]  Path to the TOML config file
            (default: GPIOJSONSVC_CONFIG, then gpiojsonsvc.toml)

Options:
      --mock       Use the file-backed mock GPIO backend
  -h, --help       Print help
  -V, --version    Print version

Environment:
  GPIOJSONSVC_CONFIG     Config file path when CONFIG is omitted
  GPIOJSONSVC_MOCK_LOG   Append chip XML dumps after mock writes
";

/// Command-line options for the Unix-socket service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    /// Path to the TOML config file.
    pub config: Option<PathBuf>,
    /// Use the mock GPIO backend. Mapped `device` values are one-chip XML files.
    pub mock: bool,
}

/// Result of parsing process arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseArgs {
    Help,
    Version,
    Run(Cli),
}

/// Parse argv. The first item is the program name and is ignored.
pub fn parse_args<I, S>(args: I) -> Result<ParseArgs, AppError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut mock = false;
    let mut config = None;
    let mut end_of_flags = false;
    let mut args = args.into_iter();
    let _program = args.next();

    for arg in args {
        let arg = arg.as_ref();
        if !end_of_flags {
            if arg == "--" {
                end_of_flags = true;
                continue;
            }
            if arg == "--help" || arg == "-h" {
                return Ok(ParseArgs::Help);
            }
            if arg == "--version" || arg == "-V" {
                return Ok(ParseArgs::Version);
            }
            if arg == "--mock" {
                if mock {
                    return Err(AppError::Usage("duplicate --mock".into()));
                }
                mock = true;
                continue;
            }
            if arg.to_string_lossy().starts_with('-') {
                return Err(AppError::Usage(format!(
                    "unknown option {}",
                    arg.to_string_lossy()
                )));
            }
        }
        if config.is_some() {
            return Err(AppError::Usage(format!(
                "unexpected extra argument {}",
                arg.to_string_lossy()
            )));
        }
        config = Some(PathBuf::from(arg));
    }

    Ok(ParseArgs::Run(Cli { config, mock }))
}

/// How the mock GPIO backend is selected from the CLI and environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MockMode {
    /// Real backend (currently unavailable).
    Off,
    /// Mock backend without the write log.
    On,
    /// Mock backend with a background XML write log at `path`.
    OnWithWriteLog(PathBuf),
}

impl MockMode {
    /// Resolve mock mode from `--mock` and `GPIOJSONSVC_MOCK_LOG`.
    pub fn from_cli(mock: bool) -> Result<Self, AppError> {
        Self::from_cli_and_env(mock, std::env::var_os(MOCK_LOG_ENV_VAR))
    }

    pub fn from_cli_and_env(mock: bool, log: Option<OsString>) -> Result<Self, AppError> {
        let log = log.filter(|path| !path.is_empty());
        match (mock, log) {
            (false, None) => Ok(Self::Off),
            (false, Some(_)) => Err(AppError::MockLogWithoutMock),
            (true, None) => Ok(Self::On),
            (true, Some(path)) => Ok(Self::OnWithWriteLog(PathBuf::from(path))),
        }
    }
}

/// Validated service configuration plus the runtime backend selector.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub service: ServiceConfig,
    pub mock: MockMode,
}

#[derive(Debug)]
enum SelectedBackend {
    Mock(MockBackend),
}

fn select_backend(mock: MockMode) -> Result<SelectedBackend, AppError> {
    match mock {
        MockMode::Off => Err(AppError::RealBackendUnavailable),
        MockMode::On => Ok(SelectedBackend::Mock(MockBackend::new())),
        MockMode::OnWithWriteLog(path) => {
            let backend = MockBackend::new().with_write_log(&path).map_err(|source| {
                AppError::MockWriteLogUnavailable {
                    path: path.display().to_string(),
                    source,
                }
            })?;
            Ok(SelectedBackend::Mock(backend))
        }
    }
}

pub async fn run(config: AppConfig) -> Result<(), AppError> {
    let _ = tracing_subscriber::fmt::try_init();
    match select_backend(config.mock)? {
        SelectedBackend::Mock(backend) => {
            validate_mock_chip_files(&config.service)?;
            let runtime = ServiceRuntime::new(
                Arc::new(backend),
                ServerConfig {
                    socket_path: config.service.socket.clone(),
                },
                Arc::new(config.service),
            );
            tracing::info!(pid = %std::process::id(), "running service");
            let events = SystemEvent::new();
            let signals = spawn_signal_task(events.clone())?;
            let result = runtime.serve(events).await;
            signals.abort();
            let _ = signals.await;
            tracing::info!(pid = %std::process::id(), "service shutdown");
            result
        }
    }
}

fn validate_mock_chip_files(config: &ServiceConfig) -> Result<(), AppError> {
    let mut snapshots: BTreeMap<String, MockChipSnapshot> = BTreeMap::new();
    for (pin, spec) in config.gpiod_pins() {
        let snapshot = snapshot_for_device(&mut snapshots, &spec.device)?;
        if !snapshot.lines.contains_key(&spec.line) {
            return Err(AppError::MissingLine {
                pin: pin.clone(),
                device: spec.device.clone(),
                line: spec.line,
            });
        }
    }
    Ok(())
}

fn snapshot_for_device<'a>(
    cache: &'a mut BTreeMap<String, MockChipSnapshot>,
    device: &str,
) -> Result<&'a MockChipSnapshot, AppError> {
    if !cache.contains_key(device) {
        let contents =
            std::fs::read_to_string(device).map_err(|source| AppError::UnavailableDeviceFile {
                device: device.to_owned(),
                source,
            })?;
        let snapshot = crate::gpio::mock::parse_chip_xml(&contents).map_err(|error| {
            AppError::InvalidChipFile {
                device: device.to_owned(),
                message: error.to_string(),
            }
        })?;
        cache.insert(device.to_owned(), snapshot);
    }
    Ok(cache
        .get(device)
        .expect("device snapshot was just inserted"))
}

pub struct ServiceRuntime<B: Backend + 'static> {
    backend: Arc<B>,
    transport: ServerConfig,
    session_config: Arc<dyn SessionConfig>,
    next_session_id: AtomicU64,
}

impl<B: Backend + 'static> ServiceRuntime<B> {
    pub fn new(
        backend: Arc<B>,
        transport: ServerConfig,
        session_config: Arc<dyn SessionConfig>,
    ) -> Self {
        Self {
            backend,
            transport,
            session_config,
            next_session_id: AtomicU64::new(1),
        }
    }

    async fn serve(self, events: SystemEvent) -> Result<(), AppError> {
        let (listener, bound) = BoundSocket::bind(&self.transport.socket_path)?;
        tracing::info!(socket = %self.transport.socket_path, "uds listener started");

        let mut sessions = Vec::new();
        let mut seen_seq = 0;
        let mut result = Ok(());
        loop {
            tokio::select! {
                biased;
                code = events.recv(&mut seen_seq) => {
                    if code == SystemEvent::SHUTDOWN {
                        tracing::info!("shutdown requested");
                        break;
                    }
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _addr)) => {
                            self.spawn_session(stream, &events, &mut sessions);
                        }
                        Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                        Err(err) => {
                            tracing::error!(error = %err, "listener accept failed");
                            result = Err(err.into());
                            break;
                        }
                    }
                }
            }
        }

        drop(listener);
        drop(bound);
        close_sessions(sessions, &events).await;
        result
    }

    fn spawn_session(
        &self,
        stream: UnixStream,
        events: &SystemEvent,
        sessions: &mut Vec<LiveSession>,
    ) {
        let connection = UdsConnection::from_stream(stream);
        let connection_info = connection.connection_info();
        let (writer, reader) = connection.split();

        let session_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let (handle, reactor_join) = SessionReactor::spawn_with_events(
            session_id,
            self.backend.clone(),
            self.session_config.clone(),
            writer,
            connection_info.clone(),
            events.clone(),
        );
        let reader_join = spawn_reader_task(
            handle.clone(),
            reader,
            connection_info.clone(),
            events.clone(),
        );

        sessions.push(LiveSession {
            handle,
            reader: reader_join,
            reactor: reactor_join,
            connection_info,
            session_id,
        });
    }
}

/// How long to wait for sessions to finish after shutdown before aborting them.
const SESSION_SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

struct LiveSession {
    handle: SessionHandle,
    reader: JoinHandle<()>,
    reactor: JoinHandle<()>,
    connection_info: String,
    session_id: u64,
}

async fn close_sessions(sessions: Vec<LiveSession>, events: &SystemEvent) {
    tracing::info!(sessions = sessions.len(), "closing sessions");
    events.emit(SystemEvent::SHUTDOWN);
    for session in &sessions {
        session.handle.shutdown();
    }

    let deadline = Instant::now() + SESSION_SHUTDOWN_GRACE;
    for session in sessions {
        let session_id = session.session_id;
        let connection = session.connection_info;
        join_with_deadline(session.reader, deadline).await;
        join_with_deadline(session.reactor, deadline).await;
        tracing::debug!(
            session_id,
            connection = %connection,
            "session tasks completed"
        );
    }
}

async fn join_with_deadline(mut handle: JoinHandle<()>, deadline: Instant) {
    tokio::select! {
        _ = &mut handle => {}
        _ = tokio::time::sleep_until(deadline) => {
            handle.abort();
            let _ = handle.await;
        }
    }
}

struct BoundSocket {
    path: String,
}

impl BoundSocket {
    fn bind(path: &str) -> Result<(UnixListener, Self), AppError> {
        cleanup_socket_if_exists(path)?;
        let listener = UnixListener::bind(path)?;
        Ok((
            listener,
            Self {
                path: path.to_owned(),
            },
        ))
    }
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn cleanup_socket_if_exists(path: &str) -> Result<(), AppError> {
    let socket_path = Path::new(path);
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }

    Ok(())
}

fn spawn_signal_task(events: SystemEvent) -> Result<JoinHandle<()>, AppError> {
    use tokio::signal::unix::signal;
    use tokio::signal::unix::SignalKind;

    let mut sigint = signal(SignalKind::interrupt())
        .map_err(|err| AppError::Bootstrap(format!("failed to listen for SIGINT: {err}")))?;
    let mut sigterm = signal(SignalKind::terminate())
        .map_err(|err| AppError::Bootstrap(format!("failed to listen for SIGTERM: {err}")))?;
    Ok(tokio::spawn(async move {
        tokio::select! {
            _ = sigint.recv() => tracing::debug!(signal = "SIGINT", "received shutdown signal"),
            _ = sigterm.recv() => tracing::debug!(signal = "SIGTERM", "received shutdown signal"),
        }
        events.emit(SystemEvent::SHUTDOWN);
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use std::ffi::OsString;

    use tempfile::TempDir;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;

    use super::AppConfig;
    use super::Cli;
    use super::HELP;
    use super::MOCK_LOG_ENV_VAR;
    use super::MockMode;
    use super::ParseArgs;
    use super::parse_args;
    use super::select_backend;
    use super::validate_mock_chip_files;
    use crate::config::ServiceConfig;
    use crate::error::AppError;
    use crate::gpio::mock::MockBackend;
    use crate::system_event::SystemEvent;
    use crate::transport::ServerConfig;

    const SAMPLE_XML: &str = r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input">L</line>
    <line id="7" name="line7" direction="output" drive="push_pull">L</line>
</gpiochip>"#;

    fn write_toml(dir: &TempDir, socket: &str, device: &str, line: u32) -> ServiceConfig {
        let path = dir.path().join("gpiojsonsvc.toml");
        let contents = format!(
            r#"
[service]
socket = "{socket}"

[pins.gpiod]
"gpiochip0:7" = {{ device = "{device}", line = {line} }}
"#
        );
        fs::write(&path, contents).expect("write config");
        ServiceConfig::load_from_path(&path).expect("load config")
    }

    fn parse_run(args: impl IntoIterator<Item = &'static str>) -> Cli {
        match parse_args(args).expect("parse cli") {
            ParseArgs::Run(cli) => cli,
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn cli_parses_mock_and_config_path() {
        let cli = parse_run(["gpiojsonsvc", "--mock", "/tmp/gpiojsonsvc.toml"]);
        assert!(cli.mock);
        assert_eq!(
            cli.config.as_deref(),
            Some(std::path::Path::new("/tmp/gpiojsonsvc.toml"))
        );
    }

    #[test]
    fn cli_parses_config_path_then_mock() {
        let cli = parse_run(["gpiojsonsvc", "/tmp/gpiojsonsvc.toml", "--mock"]);
        assert!(cli.mock);
        assert_eq!(
            cli.config.as_deref(),
            Some(std::path::Path::new("/tmp/gpiojsonsvc.toml"))
        );
    }

    #[test]
    fn cli_parses_mock_without_config_path() {
        let cli = parse_run(["gpiojsonsvc", "--mock"]);
        assert!(cli.mock);
        assert!(cli.config.is_none());
    }

    #[test]
    fn cli_defaults_to_real_backend_without_mock() {
        let cli = parse_run(["gpiojsonsvc"]);
        assert!(!cli.mock);
        assert!(cli.config.is_none());
        assert_eq!(
            MockMode::from_cli_and_env(cli.mock, None).expect("mode"),
            MockMode::Off
        );
    }

    #[test]
    fn cli_rejects_unknown_option() {
        let error = parse_args(["gpiojsonsvc", "--config", "/tmp/gpiojsonsvc.toml"])
            .expect_err("unknown option");
        assert!(error.to_string().contains("unknown option --config"));
    }

    #[test]
    fn cli_rejects_extra_positional() {
        let error = parse_args(["gpiojsonsvc", "a.toml", "b.toml"]).expect_err("extra");
        assert!(error.to_string().contains("unexpected extra argument"));
    }

    #[test]
    fn cli_rejects_duplicate_mock() {
        let error = parse_args(["gpiojsonsvc", "--mock", "--mock"]).expect_err("duplicate");
        assert_eq!(error.to_string(), "duplicate --mock");
    }

    #[test]
    fn cli_help_and_version_short_circuit() {
        assert_eq!(
            parse_args(["gpiojsonsvc", "--help"]).expect("help"),
            ParseArgs::Help
        );
        assert_eq!(
            parse_args(["gpiojsonsvc", "-V"]).expect("version"),
            ParseArgs::Version
        );
        assert!(HELP.contains("--mock"));
        assert!(HELP.contains("GPIOJSONSVC_MOCK_LOG"));
        assert!(HELP.contains(MOCK_LOG_ENV_VAR));
    }

    #[test]
    fn mock_mode_uses_env_log_path() {
        assert_eq!(
            MockMode::from_cli_and_env(true, None).expect("on"),
            MockMode::On
        );
        assert_eq!(
            MockMode::from_cli_and_env(true, Some(OsString::from(""))).expect("empty log"),
            MockMode::On
        );
        assert_eq!(
            MockMode::from_cli_and_env(true, Some(OsString::from("/tmp/mock-write.log")))
                .expect("log"),
            MockMode::OnWithWriteLog("/tmp/mock-write.log".into())
        );
        assert!(matches!(
            MockMode::from_cli_and_env(false, Some(OsString::from("/tmp/mock-write.log"))),
            Err(AppError::MockLogWithoutMock)
        ));
    }

    #[test]
    fn select_backend_requires_mock_until_real_backend_exists() {
        assert!(matches!(
            select_backend(MockMode::Off),
            Err(AppError::RealBackendUnavailable)
        ));
        assert!(select_backend(MockMode::On).is_ok());
        assert_eq!(
            select_backend(MockMode::Off).unwrap_err().to_string(),
            "real backend unavailable; use --mock"
        );
    }

    #[test]
    fn select_backend_enables_write_log_when_path_is_given() {
        let dir = TempDir::new().expect("tempdir");
        let log = dir.path().join("mock-write.log");
        let selected = select_backend(MockMode::OnWithWriteLog(log.clone())).expect("mock");
        match selected {
            super::SelectedBackend::Mock(backend) => {
                assert!(backend.write_log().is_some());
            }
        }
        assert!(log.exists());
    }

    #[test]
    fn select_backend_rejects_unwritable_write_log_path() {
        let dir = TempDir::new().expect("tempdir");
        let missing_parent = dir.path().join("no-such-dir").join("mock-write.log");
        let error = select_backend(MockMode::OnWithWriteLog(missing_parent.clone()))
            .expect_err("missing parent");
        match error {
            AppError::MockWriteLogUnavailable { path, .. } => {
                assert_eq!(path, missing_parent.display().to_string());
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn run_without_mock_fails_before_binding() {
        let dir = TempDir::new().expect("tempdir");
        let socket = dir.path().join("gpiojsonsvc.sock");
        let xml = dir.path().join("gpiochip0.xml");
        fs::write(&xml, SAMPLE_XML).expect("write xml");
        let service = write_toml(
            &dir,
            socket.to_str().expect("utf8"),
            xml.to_str().expect("utf8"),
            7,
        );

        let error = super::run(AppConfig {
            service,
            mock: MockMode::Off,
        })
        .await
        .expect_err("real backend");
        assert!(matches!(error, AppError::RealBackendUnavailable));
        assert!(!socket.exists());
    }

    #[test]
    fn mock_startup_rejects_missing_chip_file() {
        let dir = TempDir::new().expect("tempdir");
        let socket = dir.path().join("gpiojsonsvc.sock");
        let missing = dir.path().join("missing.xml");
        let service = write_toml(
            &dir,
            socket.to_str().expect("utf8"),
            missing.to_str().expect("utf8"),
            7,
        );
        let error = validate_mock_chip_files(&service).expect_err("missing xml");
        match error {
            AppError::UnavailableDeviceFile { device, .. } => {
                assert_eq!(device, missing.to_str().expect("utf8"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn mock_startup_rejects_invalid_chip_xml() {
        let dir = TempDir::new().expect("tempdir");
        let socket = dir.path().join("gpiojsonsvc.sock");
        let xml = dir.path().join("gpiochip0.xml");
        fs::write(&xml, "<not-a-gpiochip/>").expect("write xml");
        let service = write_toml(
            &dir,
            socket.to_str().expect("utf8"),
            xml.to_str().expect("utf8"),
            7,
        );
        let error = validate_mock_chip_files(&service).expect_err("invalid xml");
        match error {
            AppError::InvalidChipFile { device, .. } => {
                assert_eq!(device, xml.to_str().expect("utf8"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn mock_startup_rejects_unavailable_line() {
        let dir = TempDir::new().expect("tempdir");
        let socket = dir.path().join("gpiojsonsvc.sock");
        let xml = dir.path().join("gpiochip0.xml");
        fs::write(&xml, SAMPLE_XML).expect("write xml");
        let service = write_toml(
            &dir,
            socket.to_str().expect("utf8"),
            xml.to_str().expect("utf8"),
            99,
        );
        let error = validate_mock_chip_files(&service).expect_err("missing line");
        match error {
            AppError::MissingLine { pin, line, .. } => {
                assert_eq!(pin, "gpiochip0:7");
                assert_eq!(line, 99);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[tokio::test]
    async fn serve_binds_then_removes_socket_on_shutdown() {
        let dir = TempDir::new().expect("tempdir");
        let socket = dir.path().join("gpiojsonsvc.sock");
        let xml = dir.path().join("gpiochip0.xml");
        fs::write(&xml, SAMPLE_XML).expect("write xml");
        let service = write_toml(
            &dir,
            socket.to_str().expect("utf8"),
            xml.to_str().expect("utf8"),
            7,
        );
        validate_mock_chip_files(&service).expect("valid mock files");

        let stale = dir.path().join("stale.sock");
        fs::write(&stale, "not a socket").expect("stale file");
        let runtime = super::ServiceRuntime::new(
            std::sync::Arc::new(MockBackend::new()),
            ServerConfig {
                socket_path: stale.to_str().expect("utf8").to_owned(),
            },
            std::sync::Arc::new(service.clone()),
        );

        let events = SystemEvent::new();
        let serve_events = events.clone();
        let join = tokio::spawn(async move { runtime.serve(serve_events).await });

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if stale.exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("listener did not bind");

        events.emit(SystemEvent::SHUTDOWN);
        join.await.expect("runtime task").expect("runtime shutdown");
        assert!(!stale.exists());
    }

    #[tokio::test]
    async fn serve_closes_connected_clients_on_shutdown() {
        let dir = TempDir::new().expect("tempdir");
        let socket = dir.path().join("gpiojsonsvc.sock");
        let xml = dir.path().join("gpiochip0.xml");
        fs::write(&xml, SAMPLE_XML).expect("write xml");
        let service = write_toml(
            &dir,
            socket.to_str().expect("utf8"),
            xml.to_str().expect("utf8"),
            7,
        );
        validate_mock_chip_files(&service).expect("valid mock files");

        let socket_path = socket.to_str().expect("utf8").to_owned();
        let runtime = super::ServiceRuntime::new(
            std::sync::Arc::new(MockBackend::new()),
            ServerConfig {
                socket_path: socket_path.clone(),
            },
            std::sync::Arc::new(service),
        );

        let events = SystemEvent::new();
        let serve_events = events.clone();
        let join = tokio::spawn(async move { runtime.serve(serve_events).await });

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if socket.exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("listener did not bind");

        let mut client = tokio::net::UnixStream::connect(&socket)
            .await
            .expect("connect");
        client
            .write_all(
                br#"{"id":"init-1","action":"init","target":{"OUT":{"mode":"output","pin":"gpiochip0:7"}}}"#,
            )
            .await
            .expect("write init");
        client.write_all(b"\n").await.expect("write newline");

        let mut line = String::new();
        {
            let mut reader = tokio::io::BufReader::new(&mut client);
            tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
                .await
                .expect("init reply timed out")
                .expect("read init reply");
        }
        assert!(line.contains("\"status\":\"ok\""), "{line}");

        events.emit(SystemEvent::SHUTDOWN);
        tokio::time::timeout(Duration::from_secs(2), join)
            .await
            .expect("runtime did not stop while a client was connected")
            .expect("runtime task")
            .expect("runtime shutdown");
        assert!(!socket.exists());

        let mut buf = [0u8; 8];
        let n = tokio::time::timeout(Duration::from_secs(1), client.read(&mut buf))
            .await
            .expect("client read timed out")
            .expect("client read");
        assert_eq!(n, 0);
    }
}
