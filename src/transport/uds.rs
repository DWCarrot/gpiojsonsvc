use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use futures::Sink;
use futures::Stream;
use futures::stream::SplitSink;
use futures::stream::SplitStream;
use pin_project_lite::pin_project;
use thiserror::Error;
use tokio::net::UnixStream;
use tokio_util::codec::Framed;
use tokio_util::codec::LinesCodec;
use tokio_util::codec::LinesCodecError;

use crate::protocol::ProtocolError;
use crate::protocol::parse_request_line;
use crate::protocol::request::RequestMessage;
use crate::protocol::response::ResponseMessage;
use crate::protocol::serialize_response;

use super::Connection;
use super::ConnectionError;

#[derive(Debug, Error)]
pub enum UdsConnectionError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("transport error: {0}")]
    Transport(String),
}

impl ConnectionError for UdsConnectionError {
    fn is_protocol_error(&self) -> bool {
        matches!(self, UdsConnectionError::Protocol(_))
    }

    fn is_transport_error(&self) -> bool {
        !self.is_protocol_error()
    }

    fn try_into_io_error(self) -> Result<std::io::Error, Self>
    where
        Self: Sized,
    {
        match self {
            UdsConnectionError::Io(error) => Ok(error),
            other => Err(other),
        }
    }
}

pin_project! {
    pub struct UdsConnection {
        #[pin]
        framed: Framed<UnixStream, LinesCodec>,
        connection_info: String,
    }
}

impl UdsConnection {
    pub fn from_stream(stream: UnixStream) -> Self {
        let connection_info = describe_connection(&stream);
        Self {
            framed: Framed::new(stream, LinesCodec::new()),
            connection_info,
        }
    }

    pub fn split(self) -> (SplitSink<Self, ResponseMessage>, SplitStream<Self>) {
        futures::StreamExt::split(self)
    }
}

impl Connection<UdsConnectionError> for UdsConnection {
    fn connection_info(&self) -> String {
        self.connection_info.clone()
    }
}

impl Stream for UdsConnection {
    type Item = Result<RequestMessage, UdsConnectionError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        match this.framed.as_mut().poll_next(context) {
            Poll::Ready(Some(Ok(line))) => {
                let parsed = parse_request_line(&line).map_err(UdsConnectionError::from);
                Poll::Ready(Some(parsed))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(map_lines_codec_error(error)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Sink<ResponseMessage> for UdsConnection {
    type Error = UdsConnectionError;

    fn poll_ready(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        <Framed<UnixStream, LinesCodec> as Sink<String>>::poll_ready(this.framed.as_mut(), context)
            .map_err(map_lines_codec_error)
    }

    fn start_send(self: Pin<&mut Self>, item: ResponseMessage) -> Result<(), Self::Error> {
        let mut this = self.project();
        let line = serialize_response(&item).map_err(UdsConnectionError::from)?;
        <Framed<UnixStream, LinesCodec> as Sink<String>>::start_send(this.framed.as_mut(), line)
            .map_err(map_lines_codec_error)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        <Framed<UnixStream, LinesCodec> as Sink<String>>::poll_flush(this.framed.as_mut(), context)
            .map_err(map_lines_codec_error)
    }

    fn poll_close(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();
        <Framed<UnixStream, LinesCodec> as Sink<String>>::poll_close(this.framed.as_mut(), context)
            .map_err(map_lines_codec_error)
    }
}

fn map_lines_codec_error(error: LinesCodecError) -> UdsConnectionError {
    match error {
        LinesCodecError::Io(error) => UdsConnectionError::Io(error),
        LinesCodecError::MaxLineLengthExceeded => {
            UdsConnectionError::Transport("maximum line length exceeded".to_owned())
        }
    }
}

fn describe_connection(stream: &UnixStream) -> String {
    let local = stream
        .local_addr()
        .ok()
        .and_then(|addr| addr.as_pathname().map(|path| path.display().to_string()))
        .unwrap_or_else(|| "unnamed".to_owned());
    let peer = stream
        .peer_addr()
        .ok()
        .and_then(|addr| addr.as_pathname().map(|path| path.display().to_string()))
        .unwrap_or_else(|| "unnamed".to_owned());
    format!("uds local={local} peer={peer}")
}

#[cfg(test)]
mod tests {
    use futures::SinkExt;
    use futures::StreamExt;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::io::BufReader;
    use tokio::net::UnixStream;

    use crate::protocol::request::RequestPayload;
    use crate::protocol::response::ResponseMessage;

    use super::UdsConnection;

    #[tokio::test]
    async fn uds_connection_parses_and_serializes_lines() {
        let (server_stream, mut client_stream) = UnixStream::pair().expect("unix stream pair");
        let mut connection = UdsConnection::from_stream(server_stream);

        client_stream
            .write_all(b"{\"id\":\"init-1\",\"action\":\"init\",\"target\":{}}\n")
            .await
            .expect("write request line");

        let inbound = connection
            .next()
            .await
            .expect("stream item")
            .expect("parse request");
        assert_eq!(inbound.id, "init-1");
        match inbound.payload {
            RequestPayload::Init { target } => assert!(target.is_empty()),
            other => panic!("expected init payload, got {other:?}"),
        }

        connection
            .send(ResponseMessage::ok("init-1"))
            .await
            .expect("send response");

        let mut reader = BufReader::new(client_stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read response");
        assert!(line.ends_with('\n'));
        assert!(line.contains("\"id\":\"init-1\""));
        assert!(line.contains("\"status\":\"ok\""));
    }
}
