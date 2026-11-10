use std::io;
use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::sync::Arc;

use nix::errno::Errno;

use crate::dev_fuse::DevFuse;
use crate::passthrough::BackingId;

/// A raw communication channel to the FUSE kernel driver
#[derive(Debug, Clone)]
pub(crate) struct Channel(Endpoint);

#[derive(Debug, Clone)]
enum Endpoint {
    Device(Arc<DevFuse>),
    #[cfg(feature = "custom-transport")]
    Custom(Arc<dyn Transport>),
}

/// A message-oriented FUSE transport without a device descriptor.
///
/// Receive must return exactly one complete request, or an OS error. Send must
/// atomically send all slices as one reply (including notifications), or fail.
/// Calls to send may overlap each other and receive. The owner must close the
/// transport to wake blocked receivers before joining an externally mounted session.
#[cfg(feature = "custom-transport")]
pub trait Transport: std::fmt::Debug + Send + Sync + 'static {
    /// Read one complete request. Return ENODEV when the transport is closed.
    fn receive(&self, buffer: &mut [u8]) -> io::Result<usize>;
    /// Send one complete reply, preserving its message boundary.
    fn send(&self, buffers: &[io::IoSlice<'_>]) -> io::Result<()>;
}

#[cfg(not(feature = "custom-transport"))]
impl AsFd for Channel {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.device().expect("device channel").as_fd()
    }
}

impl Channel {
    /// Create a new communication channel to the kernel driver by mounting the
    /// given path. The kernel driver will delegate filesystem operations of
    /// the given path to the channel.
    pub(crate) fn new(device: Arc<DevFuse>) -> Self {
        Self(Endpoint::Device(device))
    }

    #[cfg(feature = "custom-transport")]
    pub(crate) fn custom(transport: Arc<dyn Transport>) -> Self {
        Self(Endpoint::Custom(transport))
    }

    fn device(&self) -> io::Result<&Arc<DevFuse>> {
        match &self.0 {
            Endpoint::Device(device) => Ok(device),
            #[cfg(feature = "custom-transport")]
            Endpoint::Custom(_) => Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP)),
        }
    }

    #[cfg(feature = "custom-transport")]
    pub(crate) fn try_as_fd(&self) -> io::Result<BorrowedFd<'_>> {
        Ok(self.device()?.as_fd())
    }

    /// Receives data up to the capacity of the given buffer (can block).
    fn receive(&self, buffer: &mut [u8]) -> nix::Result<usize> {
        match &self.0 {
            Endpoint::Device(device) => nix::unistd::read(device, buffer),
            #[cfg(feature = "custom-transport")]
            Endpoint::Custom(transport) => {
                let size = transport
                    .receive(buffer)
                    .map_err(|e| Errno::from_raw(e.raw_os_error().unwrap_or(libc::EIO)))?;
                if size == 0 || size > buffer.len() {
                    return Err(Errno::EIO);
                }
                Ok(size)
            }
        }
    }

    /// Receives data up to the capacity of the given buffer (can block),
    /// retrying on errors that are safe to retry (ENOENT, EINTR, EAGAIN).
    ///
    /// - ENOENT: Operation interrupted. According to FUSE, this is safe to retry.
    /// - EINTR: Interrupted system call, retry.
    /// - EAGAIN: Explicitly instructed to try again.
    pub(crate) fn receive_retrying(&self, buffer: &mut [u8]) -> nix::Result<usize> {
        loop {
            match self.receive(buffer) {
                Ok(size) => return Ok(size),
                Err(Errno::ENOENT | Errno::EINTR | Errno::EAGAIN) => continue,
                Err(err) => return Err(err),
            }
        }
    }

    /// Returns a sender object for this channel. The sender object can be
    /// used to send to the channel. Multiple sender objects can be used
    /// and they can safely be sent to other threads.
    pub(crate) fn sender(&self) -> ChannelSender {
        // Since write/writev syscalls are threadsafe, we can simply create
        // a sender by using the same file and use it in other threads.
        ChannelSender(self.clone())
    }

    /// Clone the FUSE device fd using FUSE_DEV_IOC_CLONE ioctl.
    ///
    /// This creates a new fd that can read FUSE requests independently,
    /// enabling true parallel request processing. The kernel distributes
    /// requests across all cloned fds.
    ///
    /// Requires Linux 4.5+. Returns an error on older kernels or non-Linux.
    #[cfg(target_os = "linux")]
    pub(crate) fn clone_fd(&self) -> io::Result<Channel> {
        use std::os::fd::AsRawFd;

        let new_dev = DevFuse::open()?;

        let mut source_fd = self.device()?.as_raw_fd() as u32;
        // SAFETY: fuse_dev_ioc_clone is a valid ioctl for /dev/fuse
        unsafe {
            crate::ll::ioctl::fuse_dev_ioc_clone(new_dev.as_raw_fd(), &mut source_fd)?;
        }

        Ok(Channel::new(Arc::new(new_dev)))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ChannelSender(Channel);

impl ChannelSender {
    pub(crate) fn send(&self, bufs: &[io::IoSlice<'_>]) -> io::Result<()> {
        #[cfg(feature = "custom-transport")]
        if let Endpoint::Custom(transport) = &self.0.0 {
            return transport.send(bufs);
        }
        let rc = nix::sys::uio::writev(self.0.device()?, bufs)?;
        // writev is atomic, so do not need to check how many bytes are written.
        // libfuse does not do it either
        // https://github.com/libfuse/libfuse/blob/6278995cca991978abd25ebb2c20ebd3fc9e8a13/lib/fuse_lowlevel.c#L267
        debug_assert_eq!(bufs.iter().map(|b| b.len()).sum::<usize>(), rc);
        Ok(())
    }

    pub(crate) fn open_backing(&self, fd: BorrowedFd<'_>) -> std::io::Result<BackingId> {
        BackingId::create(self.0.device()?, fd)
    }

    pub(crate) unsafe fn wrap_backing(&self, id: u32) -> BackingId {
        unsafe {
            BackingId::wrap_raw(
                self.0
                    .device()
                    .expect("backing IDs require a device channel"),
                id,
            )
        }
    }
}

#[cfg(all(test, feature = "custom-transport"))]
mod transport_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Condvar, Mutex};

    #[derive(Debug, Default)]
    struct Messages {
        requests: Mutex<(VecDeque<io::Result<Vec<u8>>>, bool)>,
        ready: Condvar,
        replies: Mutex<Vec<Vec<u8>>>,
    }

    impl Messages {
        fn push(&self, request: io::Result<Vec<u8>>) {
            self.requests.lock().unwrap().0.push_back(request);
            self.ready.notify_one();
        }

        fn close(&self) {
            self.requests.lock().unwrap().1 = true;
            self.ready.notify_all();
        }
    }

    impl Transport for Messages {
        fn receive(&self, buffer: &mut [u8]) -> io::Result<usize> {
            let mut state = self.requests.lock().unwrap();
            loop {
                if let Some(request) = state.0.pop_front() {
                    let request = request?;
                    if request.len() > buffer.len() {
                        return Err(io::Error::from_raw_os_error(libc::EMSGSIZE));
                    }
                    buffer[..request.len()].copy_from_slice(&request);
                    return Ok(request.len());
                }
                if state.1 {
                    return Err(io::Error::from_raw_os_error(libc::ENODEV));
                }
                state = self.ready.wait(state).unwrap();
            }
        }

        fn send(&self, buffers: &[io::IoSlice<'_>]) -> io::Result<()> {
            if self.requests.lock().unwrap().1 {
                return Err(io::Error::from_raw_os_error(libc::ENODEV));
            }
            self.replies.lock().unwrap().push(
                buffers
                    .iter()
                    .flat_map(|part| part.iter().copied())
                    .collect(),
            );
            Ok(())
        }
    }

    #[test]
    fn requests_keep_their_boundaries_and_retry_interrupted_receives() {
        let messages = Arc::new(Messages::default());
        messages.push(Err(io::Error::from_raw_os_error(libc::EINTR)));
        messages.push(Ok(b"first".to_vec()));
        messages.push(Ok(b"second".to_vec()));
        let channel = Channel::custom(messages.clone());
        let mut buffer = [0; 128];
        let len = channel.receive_retrying(&mut buffer).unwrap();
        assert_eq!(&buffer[..len], b"first");
        let len = channel.receive_retrying(&mut buffer).unwrap();
        assert_eq!(&buffer[..len], b"second");
        messages.close();
        assert_eq!(channel.receive_retrying(&mut buffer), Err(Errno::ENODEV));
        assert_eq!(
            channel.try_as_fd().unwrap_err().raw_os_error(),
            Some(libc::EOPNOTSUPP)
        );
    }

    #[test]
    fn replies_are_atomic_across_cloned_senders_and_propagate_close() {
        let messages = Arc::new(Messages::default());
        let sender = Channel::custom(messages.clone()).sender();
        std::thread::scope(|scope| {
            for id in 0..8u8 {
                let sender = sender.clone();
                scope.spawn(move || {
                    sender
                        .send(&[io::IoSlice::new(&[id]), io::IoSlice::new(&[id; 65536])])
                        .unwrap()
                });
            }
        });
        let replies = messages.replies.lock().unwrap();
        assert_eq!(replies.len(), 8);
        for reply in replies.iter() {
            assert_eq!(reply.len(), 65537);
            assert!(reply.iter().all(|byte| *byte == reply[0]));
        }
        messages.close();
        assert_eq!(
            sender
                .send(&[io::IoSlice::new(b"closed")])
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENODEV)
        );
    }

    fn init() -> Vec<u8> {
        // FUSE input header followed by an INIT for protocol 7.31.
        let mut request = Vec::new();
        request.extend_from_slice(&104u32.to_ne_bytes());
        request.extend_from_slice(&26u32.to_ne_bytes());
        request.extend_from_slice(&1u64.to_ne_bytes());
        request.extend_from_slice(&0u64.to_ne_bytes());
        for _ in 0..4 {
            request.extend_from_slice(&0u32.to_ne_bytes());
        }
        request.extend_from_slice(&7u32.to_ne_bytes());
        request.extend_from_slice(&31u32.to_ne_bytes());
        request.resize(104, 0);
        request
    }

    #[test]
    fn message_sessions_handshake_notify_and_stop_when_closed() {
        struct Fs;
        impl crate::Filesystem for Fs {}
        let messages = Arc::new(Messages::default());
        messages.push(Ok(init()));
        let session = crate::Session::from_transport(
            Fs,
            messages.clone(),
            crate::SessionACL::All,
            crate::Config::default(),
        )
        .unwrap();
        assert_eq!(
            session.try_as_fd().unwrap_err().raw_os_error(),
            Some(libc::EOPNOTSUPP)
        );
        let background = session.spawn().unwrap();
        background
            .notifier()
            .inval_inode(crate::INodeNo(42), 0, -1)
            .unwrap();
        let replies = messages.replies.lock().unwrap();
        assert_eq!(replies.len(), 2);
        assert_eq!(u64::from_ne_bytes(replies[0][8..16].try_into().unwrap()), 1);
        assert_eq!(i32::from_ne_bytes(replies[0][4..8].try_into().unwrap()), 0);
        assert_eq!(u64::from_ne_bytes(replies[1][8..16].try_into().unwrap()), 0);
        drop(replies);
        messages.close();
        background.join().unwrap();
    }

    #[test]
    fn message_sessions_reject_device_cloning_before_receiving() {
        struct Fs;
        impl crate::Filesystem for Fs {}
        let messages = Arc::new(Messages::default());
        let config = crate::Config {
            clone_fd: true,
            ..crate::Config::default()
        };
        let error = crate::Session::from_transport(Fs, messages, crate::SessionACL::All, config)
            .err()
            .unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::EOPNOTSUPP));
    }
}
