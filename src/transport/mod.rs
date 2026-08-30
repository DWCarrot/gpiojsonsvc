pub mod uds;

use futures::Sink;
use futures::Stream;
use futures::StreamExt;
use tokio::task::JoinHandle;

use crate::protocol::request::RequestMessage;
use crate::protocol::response::ResponseMessage;
use crate::session::ReactorCommand;
use crate::session::SessionHandle;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub socket_path: String,
}

pub trait ConnectionError: std::error::Error {
    fn is_protocol_error(&self) -> bool;
    fn is_transport_error(&self) -> bool;
    fn try_into_io_error(self) -> Result<std::io::Error, Self>
    where
        Self: Sized;
}

pub trait Connection<E>:
    Stream<Item = Result<RequestMessage, E>> + Sink<ResponseMessage, Error = E>
where
    E: ConnectionError,
{
    fn connection_info(&self) -> String;
}

pub fn spawn_reader_task<S, E>(
    session: SessionHandle,
    mut reader: S,
    connection_info: String,
) -> JoinHandle<()>
where
    S: Stream<Item = Result<RequestMessage, E>> + Send + Unpin + 'static,
    E: ConnectionError + Send + 'static,
{
    let tx = session.sender().clone();
    let session_id = session.session_id();
    tokio::spawn(async move {
        loop {
            match reader.next().await {
                Some(Ok(request)) => {
                    if tx
                        .send(ReactorCommand::InboundRequest(request))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Some(Err(error)) => {
                    tracing::warn!(
                        session_id,
                        connection = %connection_info,
                        error = %error,
                        "reader task failed; closing transport session"
                    );
                    let _ = tx.send(ReactorCommand::TransportClosed).await;
                    break;
                }
                None => {
                    let _ = tx.send(ReactorCommand::TransportClosed).await;
                    break;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fmt;
    use std::fs;
    use std::pin::Pin;
    use std::task::Context;
    use std::task::Poll;
    use std::time::Duration;

    use futures::Sink;
    use futures::stream;
    use tempfile::NamedTempFile;

    use crate::gpio::mock::MockBackend;
    use crate::protocol::request::PinSelector;
    use crate::protocol::request::RequestMessage;
    use crate::protocol::request::RequestPayload;
    use crate::protocol::request::TargetConfigRequest;
    use crate::protocol::response::ResponseMessage;
    use crate::session::SessionConfig;
    use crate::session::SessionReactor;

    use super::ConnectionError;
    use super::spawn_reader_task;

    const SAMPLE_XML: &str = r#"<gpiochip id="gpiochip0" label="mock gpiochip0">
    <line id="0" name="line0" direction="input" bias="pull_up">L</line>
    <line id="1" name="line1" direction="output" drive="push_pull">L</line>
</gpiochip>"#;

    #[derive(Debug)]
    struct TestSinkError;

    impl fmt::Display for TestSinkError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("test sink error")
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
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, item: ResponseMessage) -> Result<(), Self::Error> {
            self.tx.send(item).map_err(|_error| TestSinkError)
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
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
                    pin: PinSelector::Single("gpiochip0:1".to_owned()),
                    drive: None,
                },
            ),
        ])
    }

    fn write_chip_file(contents: &str) -> NamedTempFile {
        let file = NamedTempFile::new().expect("temp file");
        fs::write(file.path(), contents).expect("write chip xml");
        file
    }

    #[tokio::test]
    async fn reader_task_forwards_requests_then_closes_transport() {
        let chip_file = write_chip_file(SAMPLE_XML);
        let path = chip_file.path().to_str().expect("utf8 path").to_owned();
        let config: std::sync::Arc<dyn SessionConfig> = std::sync::Arc::new(BTreeMap::from([
            (
                "gpiochip0:0".to_owned(),
                crate::config::GPIODPinSpec {
                    device: path.clone(),
                    line: 0,
                },
            ),
            (
                "gpiochip0:1".to_owned(),
                crate::config::GPIODPinSpec {
                    device: path,
                    line: 1,
                },
            ),
        ]));

        let (responses_tx, mut responses_rx) = tokio::sync::mpsc::unbounded_channel();
        let writer = TestResponseSink { tx: responses_tx };
        let (handle, reactor_join) = SessionReactor::spawn(
            42,
            std::sync::Arc::new(MockBackend::new()),
            config,
            writer,
            "test://transport-reader".to_owned(),
        );

        let request = RequestMessage {
            id: "init-1".to_owned(),
            payload: RequestPayload::Init {
                target: sample_init_targets(),
            },
        };
        let reader = stream::iter(vec![Result::<RequestMessage, TestSinkError>::Ok(request)]);
        let reader_join = spawn_reader_task(handle, reader, "test://transport-reader".to_owned());

        let response = tokio::time::timeout(Duration::from_secs(2), responses_rx.recv())
            .await
            .expect("response timed out")
            .expect("response channel closed");
        assert_eq!(response, ResponseMessage::ok("init-1"));

        tokio::time::timeout(Duration::from_secs(2), reader_join)
            .await
            .expect("reader task did not exit")
            .expect("reader task failed");
        tokio::time::timeout(Duration::from_secs(2), reactor_join)
            .await
            .expect("reactor task did not exit")
            .expect("reactor task failed");
    }
}
