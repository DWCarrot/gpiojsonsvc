//! End-to-end checks against the compiled `gpiojsonsvc` binary.
//!
//! These tests load temporary TOML, one-chip XML files, and a Unix socket, then
//! drive `init` → `get` → immediate `set` → stepped `set` over the live protocol.

use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::net::unix::OwnedReadHalf;
use tokio::net::unix::OwnedWriteHalf;
use tokio::process::Child;
use tokio::process::Command;

const CHIP0_XML: &str = r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input" bias="pull_up">L</line>
    <line id="7" name="line7" direction="output" drive="push_pull">L</line>
</gpiochip>
"#;

const CHIP1_XML: &str = r#"<gpiochip id="gpiochip1" label="mock gpiochip1">
    <line id="13" name="line13" direction="output" drive="push_pull">L</line>
</gpiochip>
"#;

struct Harness {
    _dir: TempDir,
    socket: PathBuf,
    chip0: PathBuf,
    chip1: PathBuf,
    child: Child,
}

impl Harness {
    async fn spawn_mock() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let socket = dir.path().join("gpiojsonsvc.sock");
        let chip0 = dir.path().join("gpiochip0.xml");
        let chip1 = dir.path().join("gpiochip1.xml");
        let config = dir.path().join("gpiojsonsvc.toml");

        std::fs::write(&chip0, CHIP0_XML).expect("write chip0");
        std::fs::write(&chip1, CHIP1_XML).expect("write chip1");
        std::fs::write(
            &config,
            format!(
                r##"
[service]
socket = "{socket}"

[pins.gpiod]
"gpiochip0:0" = {{ device = "{chip0}", line = 0 }}
"gpiochip0:7" = {{ device = "{chip0}", line = 7 }}
"GPIO1_B5" = {{ device = "{chip1}", line = 13 }}
"##,
                socket = socket.display(),
                chip0 = chip0.display(),
                chip1 = chip1.display(),
            ),
        )
        .expect("write config");

        let child = spawn_service(["--mock", config.to_str().expect("utf8")]);
        wait_for_socket(&socket).await;
        Self {
            _dir: dir,
            socket,
            chip0,
            chip1,
            child,
        }
    }

    async fn connect(&self) -> Client {
        let stream = UnixStream::connect(&self.socket)
            .await
            .expect("connect to service socket");
        let (reader, writer) = stream.into_split();
        Client {
            writer,
            lines: BufReader::new(reader).lines(),
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

struct Client {
    writer: OwnedWriteHalf,
    lines: tokio::io::Lines<BufReader<OwnedReadHalf>>,
}

impl Client {
    async fn send(&mut self, request: &Value) {
        let encoded = serde_json::to_string(request).expect("serialize request");
        self.writer
            .write_all(encoded.as_bytes())
            .await
            .expect("write request");
        self.writer.write_all(b"\n").await.expect("write newline");
        self.writer.flush().await.expect("flush request");
    }

    async fn recv_matching(&mut self, request_id: &str) -> Value {
        loop {
            let line = tokio::time::timeout(Duration::from_secs(3), self.lines.next_line())
                .await
                .expect("response timed out")
                .expect("read response")
                .expect("server closed the connection");
            let payload: Value = serde_json::from_str(&line).expect("parse response json");
            if payload.get("status").and_then(Value::as_str) == Some("event") {
                continue;
            }
            assert_eq!(
                payload.get("id").and_then(Value::as_str),
                Some(request_id),
                "uncorrelated response: {payload}"
            );
            return payload;
        }
    }

    async fn rpc(&mut self, request: &Value) -> Value {
        let request_id = request
            .get("id")
            .and_then(Value::as_str)
            .expect("request id")
            .to_owned();
        self.send(request).await;
        self.recv_matching(&request_id).await
    }
}

fn bin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gpiojsonsvc"));
    command
        .env_remove("GPIOJSONSVC_CONFIG")
        .env_remove("GPIOJSONSVC_MOCK_LOG");
    command
}

fn spawn_service<'a>(args: impl IntoIterator<Item = &'a str>) -> Child {
    bin()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn gpiojsonsvc")
}

async fn wait_for_socket(path: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if UnixStream::connect(path).await.is_ok() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "service did not accept on {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn write_minimal_config(dir: &TempDir) -> PathBuf {
    let socket = dir.path().join("gpiojsonsvc.sock");
    let xml = dir.path().join("gpiochip0.xml");
    let config = dir.path().join("gpiojsonsvc.toml");
    std::fs::write(&xml, CHIP0_XML).expect("write xml");
    std::fs::write(
        &config,
        format!(
            r##"
[service]
socket = "{socket}"

[pins.gpiod]
"gpiochip0:7" = {{ device = "{xml}", line = 7 }}
"##,
            socket = socket.display(),
            xml = xml.display(),
        ),
    )
    .expect("write config");
    config
}

fn line_level(xml: &str, line_id: &str) -> char {
    let marker = format!(r#"id="{line_id}""#);
    let start = xml.find(&marker).expect("line id in xml");
    let slice = &xml[start..];
    let close = slice.find("</line>").expect("line close tag");
    slice[..close]
        .chars()
        .rev()
        .find(|ch| *ch == 'H' || *ch == 'L')
        .expect("persisted H or L")
}

async fn wait_for_line_level(path: &Path, line_id: &str, expected: char) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let xml = std::fs::read_to_string(path).expect("read chip xml");
        if line_level(&xml, line_id) == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for line {line_id} to become {expected} in {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn cli_without_mock_fails_before_binding() {
    let dir = TempDir::new().expect("tempdir");
    let socket = dir.path().join("gpiojsonsvc.sock");
    let config = write_minimal_config(&dir);

    let output = bin()
        .args([config.to_str().expect("utf8")])
        .output()
        .await
        .expect("run without --mock");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("real backend unavailable; use --mock"),
        "unexpected stderr: {stderr}"
    );
    assert!(!socket.exists());
}

#[tokio::test]
async fn cli_mock_flag_is_required_even_with_env_config() {
    let dir = TempDir::new().expect("tempdir");
    let config = write_minimal_config(&dir);

    let output = bin()
        .env("GPIOJSONSVC_CONFIG", config.to_str().expect("utf8"))
        .output()
        .await
        .expect("run with env config");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("real backend unavailable; use --mock"));
}

#[tokio::test]
async fn cli_mock_log_env_without_mock_fails() {
    let dir = TempDir::new().expect("tempdir");
    let config = write_minimal_config(&dir);

    let output = bin()
        .args([config.to_str().expect("utf8")])
        .env("GPIOJSONSVC_MOCK_LOG", "/tmp/mock-write.log")
        .output()
        .await
        .expect("run mock log without --mock");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("GPIOJSONSVC_MOCK_LOG is set but --mock was not given"));
}

#[tokio::test]
async fn cli_help_documents_mock_and_config() {
    let output = bin().arg("--help").output().await.expect("run --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--mock"));
    assert!(stdout.contains("[CONFIG]"));
    assert!(stdout.contains("GPIOJSONSVC_MOCK_LOG"));
}

#[tokio::test]
async fn cli_mock_with_log_path_records_writes() {
    let dir = TempDir::new().expect("tempdir");
    let socket = dir.path().join("gpiojsonsvc.sock");
    let xml = dir.path().join("gpiochip0.xml");
    let config = dir.path().join("gpiojsonsvc.toml");
    let log = dir.path().join("mock-write.log");
    std::fs::write(&xml, CHIP0_XML).expect("write xml");
    std::fs::write(
        &config,
        format!(
            r##"
[service]
socket = "{socket}"

[pins.gpiod]
"gpiochip0:7" = {{ device = "{xml}", line = 7 }}
"##,
            socket = socket.display(),
            xml = xml.display(),
        ),
    )
    .expect("write config");

    let mut child = bin()
        .args(["--mock", config.to_str().expect("utf8")])
        .env("GPIOJSONSVC_MOCK_LOG", log.to_str().expect("utf8"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn gpiojsonsvc");
    wait_for_socket(&socket).await;

    let stream = UnixStream::connect(&socket)
        .await
        .expect("connect to service socket");
    let (reader, writer) = stream.into_split();
    let mut client = Client {
        writer,
        lines: BufReader::new(reader).lines(),
    };

    let init = client
        .rpc(&json!({
            "id": "1",
            "action": "init",
            "target": {
                "OUT": { "mode": "output", "pin": "gpiochip0:7" }
            }
        }))
        .await;
    assert_eq!(init["status"], "ok");

    let set_out = client
        .rpc(&json!({
            "id": "2",
            "action": "set",
            "target": { "OUT": 1 }
        }))
        .await;
    assert_eq!(set_out["status"], "ok");
    wait_for_line_level(&xml, "7", 'H').await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let content = std::fs::read_to_string(&log).unwrap_or_default();
        if content.contains("<gpiochip") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for mock write log at {}",
            log.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let _ = child.start_kill();
}

#[tokio::test]
async fn uds_init_get_immediate_set_and_stepped_set_persist_mock_state() {
    let mut harness = Harness::spawn_mock().await;
    let mut client = harness.connect().await;

    let init = client
        .rpc(&json!({
            "id": "1",
            "action": "init",
            "target": {
                "IN": { "mode": "input", "pin": "gpiochip0:0" },
                "OUT": { "mode": "output", "pin": "gpiochip0:7" },
                "LED": { "mode": "output", "pin": "GPIO1_B5" }
            }
        }))
        .await;
    assert_eq!(init["id"], "1");
    assert_eq!(init["status"], "ok");

    let get_in = client
        .rpc(&json!({ "id": "2", "action": "get", "target": "IN" }))
        .await;
    assert_eq!(get_in["id"], "2");
    assert_eq!(get_in["status"], "pin_value");
    assert_eq!(get_in["value"], 0);

    let set_out = client
        .rpc(&json!({
            "id": "3",
            "action": "set",
            "target": { "OUT": 1 }
        }))
        .await;
    assert_eq!(set_out["id"], "3");
    assert_eq!(set_out["status"], "ok");

    wait_for_line_level(&harness.chip0, "7", 'H').await;
    let chip0 = std::fs::read_to_string(&harness.chip0).expect("read chip0 after immediate set");
    assert_eq!(line_level(&chip0, "0"), 'L');

    let get_in_again = client
        .rpc(&json!({ "id": "4", "action": "get", "target": "IN" }))
        .await;
    assert_eq!(get_in_again["id"], "4");
    assert_eq!(get_in_again["status"], "pin_value");
    assert_eq!(get_in_again["value"], 0);

    let stepped_request = json!({
        "id": "5",
        "action": "set",
        "target": [
            { "LED": 1 },
            { "lag": 300, "LED": 0 }
        ]
    });
    client.send(&stepped_request).await;
    wait_for_line_level(&harness.chip1, "13", 'H').await;
    let stepped = client.recv_matching("5").await;
    assert_eq!(stepped["id"], "5");
    assert_eq!(stepped["status"], "ok");
    wait_for_line_level(&harness.chip1, "13", 'L').await;

    let chip0_after =
        std::fs::read_to_string(&harness.chip0).expect("read chip0 after stepped set");
    assert_eq!(
        line_level(&chip0_after, "7"),
        'H',
        "stepped set on chip1 must not rewrite chip0"
    );

    let _ = harness.child.start_kill();
}

#[tokio::test]
async fn uds_unmapped_pin_returns_correlated_error() {
    let mut harness = Harness::spawn_mock().await;
    let mut client = harness.connect().await;

    let response = client
        .rpc(&json!({
            "id": "init-unmapped",
            "action": "init",
            "target": {
                "GHOST": { "mode": "output", "pin": "not-in-config" }
            }
        }))
        .await;
    assert_eq!(response["id"], "init-unmapped");
    assert_eq!(response["status"], "error");
    let error = response["error"].as_str().expect("error string");
    assert!(
        error.contains("not-in-config") || error.to_lowercase().contains("unmapped"),
        "unexpected error: {error}"
    );

    let _ = harness.child.start_kill();
}

#[tokio::test]
async fn sigint_exits_while_a_client_is_connected() {
    let mut harness = Harness::spawn_mock().await;
    let mut client = harness.connect().await;
    let init = client
        .rpc(&json!({
            "id": "init-1",
            "action": "init",
            "target": {
                "OUT": { "mode": "output", "pin": "gpiochip0:7" }
            }
        }))
        .await;
    assert_eq!(init["status"], "ok");
    let _connected = client;

    let pid = harness.child.id().expect("child pid");
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .expect("send SIGINT");

    let status = tokio::time::timeout(Duration::from_secs(3), harness.child.wait())
        .await
        .expect("service did not exit after SIGINT")
        .expect("wait for service");
    assert!(
        status.success(),
        "expected graceful exit after SIGINT, got {status:?}"
    );
    assert!(
        !harness.socket.exists(),
        "socket path should be removed on shutdown"
    );
}
