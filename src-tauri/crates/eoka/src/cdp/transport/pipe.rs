//! Chrome's DevTools pipe: the transport for a Chrome this crate starts on Unix.
//!
//! Chrome started with `--remote-debugging-pipe` reads CDP messages on its
//! file descriptor 3 and writes them on 4. Each message is JSON ended by a
//! NUL byte, which JSON text never contains. The pipe reaches the browser
//! endpoint, as the browser WebSocket does, so flat sessions work the same
//! way. A DevTools port listens on `127.0.0.1` without a password, and any
//! user or process on the machine can connect to it; only this process holds
//! the pipe.

use std::future::poll_fn;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::pin::Pin;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

use tokio::io::{AsyncReadExt, AsyncWrite};
use tokio::net::unix::pipe::{Receiver, Sender};

use super::MAX_MESSAGE_BYTES;

/// Bytes asked of each read.
const READ_CHUNK: usize = 64 * 1024;
/// A buffer a large message grew past this is released after it, so one
/// screenshot doesn't hold its size for the browser's lifetime.
const KEEP_CAPACITY: usize = 1024 * 1024;

/// This process's ends of the two pipes a Chrome was started with.
pub(crate) struct DevToolsPipe {
    /// Written here, read by Chrome on its fd 3.
    pub(crate) to_chrome: OwnedFd,
    /// Written by Chrome on its fd 4, read here.
    pub(crate) from_chrome: OwnedFd,
}

/// Chrome's ends of the pipes, which become its fds 3 and 4.
pub(crate) struct ChromeEnds {
    read: OwnedFd,
    write: OwnedFd,
    /// Originals of ends that were numbered below 5, open until the spawn is
    /// over: while 3 and 4 are taken, nothing `Command` opens for the spawn
    /// can be given those numbers and then be replaced by `dup2` in the child.
    _low: Vec<OwnedFd>,
}

/// The pipes for one Chrome: this process's ends, and the ends it gets.
/// Every descriptor is close-on-exec, so no other child inherits them.
pub(crate) fn devtools_pipes() -> io::Result<(DevToolsPipe, ChromeEnds)> {
    let (chrome_read, to_chrome) = cloexec_pipe()?;
    let (from_chrome, chrome_write) = cloexec_pipe()?;
    let mut low = Vec::new();
    let read = above_four(chrome_read, &mut low)?;
    let write = above_four(chrome_write, &mut low)?;
    Ok((
        DevToolsPipe {
            to_chrome,
            from_chrome,
        },
        ChromeEnds {
            read,
            write,
            _low: low,
        },
    ))
}

impl ChromeEnds {
    /// Chrome's read and write ends, for a test that plays Chrome.
    #[cfg(test)]
    pub(super) fn into_fds(self) -> (OwnedFd, OwnedFd) {
        (self.read, self.write)
    }

    /// Makes these ends the child's fds 3 and 4.
    pub(crate) fn hand_to(&self, command: &mut Command) {
        use std::os::unix::process::CommandExt;

        let read = self.read.as_raw_fd();
        let write = self.write.as_raw_fd();
        // SAFETY: between fork and exec the closure only calls dup2, which is
        // async-signal-safe, and reads errno. Both sources are above 4, so
        // neither dup2 replaces the other's source, and stdio set-up (0-2)
        // doesn't touch them. dup2 clears close-on-exec on 3 and 4 only.
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(read, 3) == -1 || libc::dup2(write, 4) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn cloexec_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: `fds` has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2 succeeded, so both are new descriptors only we own.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Without pipe2 (macOS), a fork in another thread before close-on-exec is
/// set can inherit these, as with every pipe `std` makes there.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn cloexec_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: `fds` has room for the two descriptors pipe writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe succeeded, so both are new descriptors only we own.
    let ends = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&ends.0, &ends.1] {
        // SAFETY: `fd` is open; F_SETFD only changes its flags.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(ends)
}

/// `fd`, or a close-on-exec copy of it numbered above 4 if it isn't, so it
/// is never both the source and the target of a `dup2` (which would leave
/// close-on-exec set). The original goes to `low`.
fn above_four(fd: OwnedFd, low: &mut Vec<OwnedFd>) -> io::Result<OwnedFd> {
    if fd.as_raw_fd() > 4 {
        return Ok(fd);
    }
    // SAFETY: `fd` is open; F_DUPFD_CLOEXEC returns a new descriptor or -1.
    let raised = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 5) };
    if raised == -1 {
        return Err(io::Error::last_os_error());
    }
    low.push(fd);
    // SAFETY: fcntl returned a new descriptor that only we own.
    Ok(unsafe { OwnedFd::from_raw_fd(raised) })
}

/// Our write end, which shutdown can close without the transport's writer
/// lock. Chrome then reads EOF on fd 3, and exits.
pub(super) struct PipeLink {
    state: Mutex<LinkState>,
}

struct LinkState {
    /// `None` once shut down.
    sender: Option<Sender>,
    /// The writer waiting for room in the pipe, woken by shutdown.
    waiting: Option<Waker>,
}

impl PipeLink {
    pub(super) fn new(sender: Sender) -> Self {
        Self {
            state: Mutex::new(LinkState {
                sender: Some(sender),
                waiting: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, LinkState> {
        // Every critical section leaves the state whole.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn poll_write(&self, cx: &mut Context<'_>, bytes: &[u8]) -> Poll<io::Result<usize>> {
        let mut state = self.lock();
        let Some(sender) = state.sender.as_mut() else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the DevTools pipe is closed",
            )));
        };
        let written = Pin::new(sender).poll_write(cx, bytes);
        if written.is_pending() {
            state.waiting = Some(cx.waker().clone());
        }
        written
    }

    /// Closes our write end, and wakes a writer waiting for room.
    pub(super) fn shutdown(&self) {
        let (sender, waiting) = {
            let mut state = self.lock();
            (state.sender.take(), state.waiting.take())
        };
        drop(sender);
        if let Some(waiting) = waiting {
            waiting.wake();
        }
    }
}

/// The transport's writer over the pipe.
pub(super) struct PipeWriter {
    link: Arc<PipeLink>,
    /// Framed messages not yet written. Queued whole, so a cancelled flush
    /// leaves the rest of its message to the next flush, ahead of anything
    /// newer, as the WebSocket's buffer does.
    queued: Vec<u8>,
    /// How much of `queued` is written.
    written: usize,
}

impl PipeWriter {
    pub(super) fn new(link: Arc<PipeLink>) -> Self {
        Self {
            link,
            queued: Vec::new(),
            written: 0,
        }
    }

    /// Queues one message, ended by its NUL.
    pub(super) fn queue(&mut self, message: &str) {
        self.queued.reserve(message.len() + 1);
        self.queued.extend_from_slice(message.as_bytes());
        self.queued.push(0);
    }

    /// Writes everything queued. Cancel-safe: what isn't written stays
    /// queued, in order.
    pub(super) async fn flush(&mut self) -> io::Result<()> {
        while self.written < self.queued.len() {
            let link = &self.link;
            let rest = &self.queued[self.written..];
            let written = poll_fn(|cx| link.poll_write(cx, rest)).await?;
            if written == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            self.written += written;
        }
        self.queued.clear();
        self.written = 0;
        if self.queued.capacity() > KEEP_CAPACITY {
            self.queued = Vec::new();
        }
        Ok(())
    }

    /// Closes our write end: Chrome reads EOF.
    pub(super) fn close(&mut self) {
        self.link.shutdown();
    }
}

/// The transport's reader over the pipe.
pub(super) struct PipeReader {
    receiver: Receiver,
    buffer: Vec<u8>,
    /// Where the next message starts in `buffer`.
    start: usize,
    /// How far `buffer` was searched for the NUL that ends it.
    searched: usize,
}

impl PipeReader {
    pub(super) fn new(receiver: Receiver) -> Self {
        Self {
            receiver,
            buffer: Vec::new(),
            start: 0,
            searched: 0,
        }
    }

    /// The next message, without its NUL, or why the pipe ended.
    pub(super) async fn next(&mut self) -> std::result::Result<Vec<u8>, String> {
        loop {
            if let Some(offset) = self.buffer[self.searched..]
                .iter()
                .position(|&byte| byte == 0)
            {
                let end = self.searched + offset;
                let message = if self.start == 0 && end + 1 == self.buffer.len() {
                    // The buffer holds just this message: hand it over whole.
                    let mut message = std::mem::take(&mut self.buffer);
                    message.truncate(end);
                    self.start = 0;
                    message
                } else {
                    let message = self.buffer[self.start..end].to_vec();
                    self.start = end + 1;
                    message
                };
                self.searched = self.start;
                return Ok(message);
            }
            self.searched = self.buffer.len();
            if self.buffer.len() - self.start > MAX_MESSAGE_BYTES {
                return Err(format!(
                    "CDP message over the {} MiB limit",
                    MAX_MESSAGE_BYTES >> 20
                ));
            }
            if self.start > 0 {
                self.buffer.drain(..self.start);
                self.searched -= self.start;
                self.start = 0;
            }
            if self.buffer.capacity() > KEEP_CAPACITY && self.buffer.len() < READ_CHUNK {
                self.buffer.shrink_to(2 * READ_CHUNK);
            }
            self.buffer.reserve(READ_CHUNK);
            match self.receiver.read_buf(&mut self.buffer).await {
                Ok(0) if self.buffer.is_empty() => {
                    return Err("Chrome closed the DevTools pipe".to_string())
                }
                Ok(0) => return Err("Chrome closed the DevTools pipe inside a message".to_string()),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("DevTools pipe read error: {error}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn reader_and_writer() -> (PipeReader, Sender) {
        let (read, write) = cloexec_pipe().unwrap();
        (
            PipeReader::new(Receiver::from_owned_fd(read).unwrap()),
            Sender::from_owned_fd(write).unwrap(),
        )
    }

    #[tokio::test]
    async fn messages_split_on_nul_across_and_within_reads() {
        let (mut reader, mut peer) = reader_and_writer();
        peer.write_all(b"{\"a\":1}\0{\"b\"").await.unwrap();
        assert_eq!(reader.next().await.unwrap(), b"{\"a\":1}");
        peer.write_all(b":2}\0{\"c\":3}\0{\"d\":4}\0")
            .await
            .unwrap();
        assert_eq!(reader.next().await.unwrap(), b"{\"b\":2}");
        assert_eq!(reader.next().await.unwrap(), b"{\"c\":3}");
        assert_eq!(reader.next().await.unwrap(), b"{\"d\":4}");
        drop(peer);
        assert_eq!(
            reader.next().await.unwrap_err(),
            "Chrome closed the DevTools pipe"
        );
    }

    #[tokio::test]
    async fn a_message_larger_than_the_pipe_and_the_read_chunk_arrives_whole() {
        let (mut reader, mut peer) = reader_and_writer();
        let large = format!("{{\"data\":\"{}\"}}", "x".repeat(3 * READ_CHUNK + 17));
        let framed = [large.as_bytes(), b"\0"].concat();
        let (written, read) = tokio::join!(peer.write_all(&framed), reader.next());
        written.unwrap();
        assert_eq!(read.unwrap(), large.as_bytes());
    }

    #[tokio::test]
    async fn eof_inside_a_message_is_reported() {
        let (mut reader, mut peer) = reader_and_writer();
        peer.write_all(b"{\"partial\":").await.unwrap();
        drop(peer);
        assert_eq!(
            reader.next().await.unwrap_err(),
            "Chrome closed the DevTools pipe inside a message"
        );
    }

    #[tokio::test]
    async fn queued_messages_are_framed_and_flushed_in_order() {
        let (read, write) = cloexec_pipe().unwrap();
        let link = Arc::new(PipeLink::new(Sender::from_owned_fd(write).unwrap()));
        let mut writer = PipeWriter::new(Arc::clone(&link));
        let mut reader = PipeReader::new(Receiver::from_owned_fd(read).unwrap());
        writer.queue("{\"id\":1}");
        writer.queue("{\"id\":2}");
        writer.flush().await.unwrap();
        assert_eq!(reader.next().await.unwrap(), b"{\"id\":1}");
        assert_eq!(reader.next().await.unwrap(), b"{\"id\":2}");
        writer.close();
        assert!(reader.next().await.is_err(), "shutdown must close the pipe");
        writer.queue("{\"id\":3}");
        assert_eq!(
            writer.flush().await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[tokio::test]
    async fn shutdown_wakes_a_writer_waiting_on_a_full_pipe() {
        let (_read, write) = cloexec_pipe().unwrap();
        let link = Arc::new(PipeLink::new(Sender::from_owned_fd(write).unwrap()));
        let mut writer = PipeWriter::new(Arc::clone(&link));
        // Far more than any pipe buffer, never read.
        writer.queue(&"x".repeat(16 * 1024 * 1024));
        let flush = tokio::spawn(async move { writer.flush().await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!flush.is_finished(), "the full pipe must hold the writer");
        link.shutdown();
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), flush)
            .await
            .expect("shutdown must wake the waiting writer")
            .unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn chrome_ends_are_numbered_above_four_and_close_on_exec() {
        let (ours, chrome) = devtools_pipes().unwrap();
        for fd in [
            &chrome.read,
            &chrome.write,
            &ours.to_chrome,
            &ours.from_chrome,
        ] {
            // SAFETY: `fd` is open; F_GETFD only reads its flags.
            let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
            assert!(flags & libc::FD_CLOEXEC != 0);
        }
        assert!(chrome.read.as_raw_fd() > 4 && chrome.write.as_raw_fd() > 4);
    }
}
