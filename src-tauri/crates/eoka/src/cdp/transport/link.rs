//! What a transport's messages travel on: Chrome's DevTools WebSocket, or
//! (Unix) the pipe Chrome was started with.

#[cfg(unix)]
use super::pipe::{DevToolsPipe, PipeLink, PipeReader, PipeWriter};
use super::reader::write_automatic;
use super::*;
use tokio_tungstenite::tungstenite::Bytes;

/// A connection to Chrome, before the transport splits it.
pub(crate) enum Link {
    /// The browser WebSocket on a DevTools port.
    WebSocket(Box<WsStream>),
    /// The pipe Chrome was started with (`--remote-debugging-pipe`).
    #[cfg(unix)]
    Pipe(DevToolsPipe),
}

impl Link {
    /// The writer, the reader, and what tears the connection down without
    /// the writer.
    pub(super) fn split(self) -> Result<(Writer, Reader, Teardown)> {
        match self {
            Self::WebSocket(stream) => {
                let socket = duplicate_socket(&stream)?;
                let (sink, source) = (*stream).split();
                Ok((
                    Writer::WebSocket(sink),
                    Reader::WebSocket(source),
                    Teardown::Socket(socket),
                ))
            }
            #[cfg(unix)]
            Self::Pipe(pipe) => {
                let sender = tokio::net::unix::pipe::Sender::from_owned_fd(pipe.to_chrome)
                    .map_err(|error| {
                        Error::transport_io("Failed to use the DevTools pipe", error)
                    })?;
                let receiver = tokio::net::unix::pipe::Receiver::from_owned_fd(pipe.from_chrome)
                    .map_err(|error| {
                        Error::transport_io("Failed to use the DevTools pipe", error)
                    })?;
                let link = Arc::new(PipeLink::new(sender));
                Ok((
                    Writer::Pipe(PipeWriter::new(Arc::clone(&link))),
                    Reader::Pipe(PipeReader::new(receiver)),
                    Teardown::Pipe(link),
                ))
            }
        }
    }
}

/// Ends a connection independently of its writer lock, even under
/// backpressure: shuts the socket down, or closes the pipe's write end.
#[derive(Clone)]
pub(super) enum Teardown {
    Socket(Arc<std::net::TcpStream>),
    #[cfg(unix)]
    Pipe(Arc<PipeLink>),
}

impl Teardown {
    pub(super) fn shutdown(&self) {
        match self {
            // Shutdown affects every duplicated descriptor, including the
            // socket retained by SplitSink. Repeated shutdown after EOF is
            // harmless.
            Self::Socket(socket) => {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
            // Chrome reads EOF on its fd 3 and exits; a writer waiting for
            // room in the pipe fails at once.
            #[cfg(unix)]
            Self::Pipe(link) => link.shutdown(),
        }
    }
}

/// Tears the connection down when dropped. Each guard does so
/// independently of other owners of the connection.
pub(super) struct ShutdownGuard(pub(super) Teardown);

impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

/// A reply the reader sends on its own.
pub(super) enum Automatic {
    /// A CDP command, such as `Fetch.continueRequest`.
    Command(String),
    /// The answer to a WebSocket ping. Split streams don't answer them.
    Pong(Bytes),
}

/// The write half: WebSocket frames, or NUL-ended messages on the pipe.
pub(super) enum Writer {
    WebSocket(WsSink),
    #[cfg(unix)]
    Pipe(PipeWriter),
}

impl Writer {
    /// Waits until a message can be taken: the WebSocket sink's readiness,
    /// or the pipe having written what an earlier, cancelled flush left.
    pub(super) async fn ready(&mut self) -> Result<()> {
        match self {
            Self::WebSocket(sink) => poll_fn(|cx| sink.poll_ready_unpin(cx))
                .await
                .map_err(websocket_write_error),
            #[cfg(unix)]
            Self::Pipe(pipe) => pipe.flush().await.map_err(pipe_write_error),
        }
    }

    /// Takes one message; `flush` sends it.
    pub(super) fn start_send(&mut self, text: String) -> Result<()> {
        match self {
            Self::WebSocket(sink) => sink
                .start_send_unpin(Message::Text(text.into()))
                .map_err(websocket_write_error),
            #[cfg(unix)]
            Self::Pipe(pipe) => {
                pipe.queue(&text);
                Ok(())
            }
        }
    }

    pub(super) async fn flush(&mut self) -> Result<()> {
        match self {
            Self::WebSocket(sink) => sink.flush().await.map_err(websocket_write_error),
            #[cfg(unix)]
            Self::Pipe(pipe) => pipe.flush().await.map_err(pipe_write_error),
        }
    }

    /// Sends one message.
    pub(super) async fn send(&mut self, text: String) -> Result<()> {
        self.ready().await?;
        self.start_send(text)?;
        self.flush().await
    }

    /// Sends a reply the reader makes itself.
    pub(super) async fn send_automatic(&mut self, reply: Automatic) -> Result<()> {
        match (self, reply) {
            (writer, Automatic::Command(text)) => writer.send(text).await,
            (Self::WebSocket(sink), Automatic::Pong(payload)) => sink
                .send(Message::Pong(payload))
                .await
                .map_err(websocket_write_error),
            // Pipes carry no pings.
            #[cfg(unix)]
            (Self::Pipe(_), Automatic::Pong(_)) => Ok(()),
        }
    }

    /// Ends the connection from this side: a WebSocket close frame, or EOF
    /// on Chrome's end of the pipe. Later writes fail.
    pub(super) async fn close(&mut self) -> Result<()> {
        match self {
            Self::WebSocket(sink) => sink
                .send(Message::Close(None))
                .await
                .map_err(websocket_write_error),
            #[cfg(unix)]
            Self::Pipe(pipe) => {
                pipe.close();
                Ok(())
            }
        }
    }
}

fn websocket_write_error(error: tokio_tungstenite::tungstenite::Error) -> Error {
    Error::transport(format!("WebSocket write failed: {error}"))
}

#[cfg(unix)]
fn pipe_write_error(error: std::io::Error) -> Error {
    Error::transport(format!("DevTools pipe write failed: {error}"))
}

/// The read half.
pub(super) enum Reader {
    WebSocket(WsSource),
    #[cfg(unix)]
    Pipe(PipeReader),
}

/// One message as read, before it is checked to be UTF-8.
pub(super) enum Incoming {
    WebSocket(Message),
    #[cfg(unix)]
    Pipe(Vec<u8>),
}

impl Incoming {
    pub(super) fn len(&self) -> usize {
        match self {
            Self::WebSocket(message) => message.len(),
            #[cfg(unix)]
            Self::Pipe(bytes) => bytes.len(),
        }
    }

    /// The message's text; `None` if it isn't UTF-8.
    pub(super) fn text(&self) -> Option<&str> {
        match self {
            Self::WebSocket(message) => message.to_text().ok(),
            #[cfg(unix)]
            Self::Pipe(bytes) => std::str::from_utf8(bytes).ok(),
        }
    }
}

impl Reader {
    /// How the end of this connection is reported to pending commands.
    pub(super) fn lost(&self) -> &'static str {
        match self {
            Self::WebSocket(_) => "WebSocket connection lost",
            #[cfg(unix)]
            Self::Pipe(_) => "DevTools pipe connection lost",
        }
    }

    /// The next CDP message, answering WebSocket pings on the way, or why
    /// the connection ended.
    pub(super) async fn next(
        &mut self,
        writer: &SharedSink,
        write_timeout: Duration,
    ) -> std::result::Result<Incoming, String> {
        match self {
            Self::WebSocket(source) => loop {
                match source.next().await {
                    Some(Ok(message @ (Message::Text(_) | Message::Binary(_)))) => {
                        return Ok(Incoming::WebSocket(message))
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if let Err(error) =
                            write_automatic(writer, Automatic::Pong(payload), write_timeout).await
                        {
                            return Err(format!("WebSocket pong write failed: {}", error));
                        }
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Frame(_))) => {}
                    Some(Ok(Message::Close(_))) => return Err("WebSocket closed by server".into()),
                    Some(Err(e)) => return Err(format!("WebSocket read error: {}", e)),
                    None => return Err("WebSocket stream ended".into()),
                }
            },
            #[cfg(unix)]
            Self::Pipe(pipe) => pipe.next().await.map(Incoming::Pipe),
        }
    }
}
