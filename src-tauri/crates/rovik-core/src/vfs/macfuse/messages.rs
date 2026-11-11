//! FSKit uses MFMount's message channel, which has no device descriptor.
//! The framework is loaded directly from macFUSE's bundle at runtime.

use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void, CString};
use std::io::{self, IoSlice};
use std::sync::{mpsc, Arc, LazyLock, Mutex};
use std::time::Instant;

use fuser::{BackgroundSession, Config, Session, SessionACL, Transport};

use super::{dl_error, FuseFs, MountProblem, BUNDLE, MOUNT_TIMEOUT};

type Ref = *mut c_void;

#[derive(Debug)]
struct Api {
    create: unsafe extern "C" fn() -> Ref,
    mount: unsafe extern "C" fn(Ref, *const c_char, *const c_char, bool) -> c_int,
    receive: unsafe extern "C" fn(Ref) -> Ref,
    body: unsafe extern "C" fn(Ref, *mut *const libc::iovec) -> isize,
    send: unsafe extern "C" fn(Ref, *const libc::iovec, usize) -> isize,
    close: unsafe extern "C" fn(Ref) -> bool,
    release: unsafe extern "C" fn(Ref),
}

impl Api {
    fn load() -> Result<&'static Self, String> {
        static API: Mutex<Option<&'static Api>> = Mutex::new(None);
        let mut loaded = API.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(api) = *loaded {
            return Ok(api);
        }
        let api = Box::leak(Box::new(Self::load_uncached()?));
        *loaded = Some(api);
        Ok(api)
    }

    fn load_uncached() -> Result<Self, String> {
        let path = CString::new(format!(
            "{BUNDLE}/Contents/Frameworks/MFMount.framework/MFMount"
        ))
        .expect("constant framework path");
        let handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return Err(format!("macFUSE's MFMount framework could not be loaded: {}. Reinstall a current macFUSE release and retry", dl_error()));
        }
        let load = || -> Result<Self, String> {
            macro_rules! symbol {
                ($name:literal, $ty:ty) => {{
                    let ptr = unsafe { libc::dlsym(handle, concat!($name, "\0").as_ptr().cast()) };
                    if ptr.is_null() {
                        return Err(format!("macFUSE does not export {}", $name));
                    }
                    // SAFETY: signatures follow MFMount/MFMount.h's C API.
                    unsafe { std::mem::transmute::<*mut c_void, $ty>(ptr) }
                }};
            }
            Ok(Self {
                create: symbol!("MFChannelCreate", unsafe extern "C" fn() -> Ref),
                mount: symbol!(
                    "MFMount",
                    unsafe extern "C" fn(Ref, *const c_char, *const c_char, bool) -> c_int
                ),
                receive: symbol!("MFChannelCopyNextMessage", unsafe extern "C" fn(Ref) -> Ref),
                body: symbol!(
                    "MFMessageGetBodyBuffers",
                    unsafe extern "C" fn(Ref, *mut *const libc::iovec) -> isize
                ),
                send: symbol!(
                    "MFChannelSendMessage",
                    unsafe extern "C" fn(Ref, *const libc::iovec, usize) -> isize
                ),
                close: symbol!("MFChannelClose", unsafe extern "C" fn(Ref) -> bool),
                release: symbol!("MFRelease", unsafe extern "C" fn(Ref)),
            })
        };
        match load() {
            // Keep the framework loaded while its function pointers are used.
            Ok(api) => Ok(api),
            Err(error) => {
                unsafe { libc::dlclose(handle) };
                Err(format!("macFUSE's FSKit message API could not be loaded: {error}. Install a current macFUSE release and retry"))
            }
        }
    }
}

static MOUNT_PATHS: LazyLock<Mutex<HashSet<Vec<u8>>>> = LazyLock::new(Mutex::default);

#[derive(Debug)]
struct MountPath(Vec<u8>);

impl MountPath {
    fn reserve(path: &CString) -> Result<Self, MountProblem> {
        let path = path.as_bytes().to_vec();
        if !MOUNT_PATHS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(path.clone())
        {
            return Err(MountProblem::failed("An FSKit mount is still using or initializing this mount point; wait for it to finish and retry"));
        }
        Ok(Self(path))
    }
}

impl Drop for MountPath {
    fn drop(&mut self) {
        MOUNT_PATHS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.0);
    }
}

#[derive(Debug, Default)]
struct Lifecycle {
    mount_finished: bool,
    cancelled: bool,
    closed: bool,
}

/// Shared by the mount owner, handshake, request loop and reply senders.
#[derive(Debug)]
pub(super) struct MessageChannel {
    api: &'static Api,
    channel: Ref,
    mountpoint: CString,
    lifecycle: Mutex<Lifecycle>,
    // A timed-out mount must finish cleanup before a retry can reuse its path.
    _mount_path: Option<MountPath>,
}

// SAFETY: MFMount channels support concurrent receive/send/close. The retained
// reference outlives every operation through Arc; lifecycle serializes close.
unsafe impl Send for MessageChannel {}
unsafe impl Sync for MessageChannel {}

impl MessageChannel {
    fn close_locked(&self, lifecycle: &mut Lifecycle) {
        if !lifecycle.closed {
            if !unsafe { (self.api.close)(self.channel) } {
                log::warn!(
                    "Could not close FSKit message channel: {}",
                    io::Error::last_os_error()
                );
            }
            lifecycle.closed = true;
        }
    }

    /// Called only after an ordinary OS unmount succeeded, or on normal close.
    pub(super) fn close(&self) {
        let mut lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        self.close_locked(&mut lifecycle);
    }

    fn cancel(&self) {
        let mut lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        lifecycle.cancelled = true;
        // Closing also resolves an unopened MFMount channel and wakes readers.
        // If mounting completes later, its completion still detaches the volume.
        if lifecycle.mount_finished && !lifecycle.closed {
            self.detach_and_close(&mut lifecycle);
        } else {
            self.close_locked(&mut lifecycle);
        }
    }

    fn detach_and_close(&self, lifecycle: &mut Lifecycle) {
        // Only initialization failure/timeout uses force. Upgrade drains call
        // the parent's ordinary unmount before touching this channel.
        if unsafe { libc::unmount(self.mountpoint.as_ptr(), libc::MNT_FORCE) } != 0 {
            let error = io::Error::last_os_error();
            if !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOENT)) {
                log::warn!("Could not detach failed FSKit mount: {error}");
            }
        }
        self.close_locked(lifecycle);
    }

    fn mount(&self, options: &CString) -> Result<(), String> {
        // Quiet mode keeps setup errors in Rovik's actionable driver UI.
        let status = unsafe {
            (self.api.mount)(
                self.channel,
                self.mountpoint.as_ptr(),
                options.as_ptr(),
                true,
            )
        };
        let error = io::Error::last_os_error();
        let mut lifecycle = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        lifecycle.mount_finished = true;
        if status != 0 {
            // MFMount closes its channel before returning a mount failure.
            lifecycle.closed = true;
            return Err(match status {
                1 => "FSKit requires macOS 15.4 or later".into(),
                2 => "macFUSE could not install its helper tools; reinstall macFUSE".into(),
                3 => "macFUSE's filesystem extension is missing; reinstall macFUSE".into(),
                4 => "Enable macFUSE's filesystem extension in System Settings".into(),
                5 => "macFUSE could not create the mount point under /Volumes".into(),
                _ => format!("macFUSE's FSKit mount failed (status {status}): {error}"),
            });
        }
        if lifecycle.cancelled {
            self.detach_and_close(&mut lifecycle);
            return Err("FSKit mount was cancelled after its initialization deadline".into());
        }
        Ok(())
    }
}

impl Drop for MessageChannel {
    fn drop(&mut self) {
        self.close();
        // SAFETY: the last Arc is gone, so no operation still uses this reference.
        unsafe { (self.api.release)(self.channel) };
    }
}

impl Transport for MessageChannel {
    fn receive(&self, buffer: &mut [u8]) -> io::Result<usize> {
        let message = unsafe { (self.api.receive)(self.channel) };
        if message.is_null() {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            let mut buffers = std::ptr::null();
            let count = unsafe { (self.api.body)(message, &mut buffers) };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            if count == 0 || buffers.is_null() {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            // SAFETY: MFMessage owns this array and its body until MFRelease.
            let buffers = unsafe { std::slice::from_raw_parts(buffers, count as usize) };
            let mut offset = 0usize;
            for part in buffers {
                let end = offset
                    .checked_add(part.iov_len)
                    .filter(|end| *end <= buffer.len())
                    .ok_or_else(|| io::Error::from_raw_os_error(libc::EMSGSIZE))?;
                if part.iov_len != 0 {
                    if part.iov_base.is_null() {
                        return Err(io::Error::from_raw_os_error(libc::EIO));
                    }
                    let bytes = unsafe {
                        std::slice::from_raw_parts(part.iov_base.cast::<u8>(), part.iov_len)
                    };
                    buffer[offset..end].copy_from_slice(bytes);
                }
                offset = end;
            }
            #[cfg(test)]
            trace_request(&buffer[..offset]);
            Ok(offset)
        })();
        unsafe { (self.api.release)(message) };
        result
    }

    fn send(&self, buffers: &[IoSlice<'_>]) -> io::Result<()> {
        #[cfg(test)]
        if let Some(header) = buffers.first().and_then(|buffer| buffer.get(..16)) {
            if std::env::var("ROVIK_MOUNTED_BACKEND").as_deref() == Ok("1") {
                eprintln!(
                    "FSKit reply unique={} error={} length={}",
                    u64::from_ne_bytes(header[8..16].try_into().unwrap()),
                    i32::from_ne_bytes(header[4..8].try_into().unwrap()),
                    u32::from_ne_bytes(header[..4].try_into().unwrap())
                );
            }
        }
        let parts: Vec<libc::iovec> = buffers
            .iter()
            .map(|part| libc::iovec {
                iov_base: part.as_ptr().cast_mut().cast(),
                iov_len: part.len(),
            })
            .collect();
        let written = unsafe { (self.api.send)(self.channel, parts.as_ptr(), parts.len()) };
        if written < 0 {
            return Err(io::Error::last_os_error());
        }
        if written as usize != buffers.iter().map(|part| part.len()).sum::<usize>() {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        Ok(())
    }
}

// Native CI needs protocol diagnostics even when no application logger exists.
// Headers contain identifiers/status only; file names and payloads are omitted.
#[cfg(test)]
fn trace_request(bytes: &[u8]) {
    if std::env::var("ROVIK_MOUNTED_BACKEND").as_deref() == Ok("1") {
        if let Some(header) = bytes.get(..40) {
            eprintln!(
                "FSKit request opcode={} unique={} uid={} length={}",
                u32::from_ne_bytes(header[4..8].try_into().unwrap()),
                u64::from_ne_bytes(header[8..16].try_into().unwrap()),
                u32::from_ne_bytes(header[24..28].try_into().unwrap()),
                u32::from_ne_bytes(header[..4].try_into().unwrap())
            );
        }
    }
}

pub(super) fn mount(
    fs: FuseFs,
    mountpoint: CString,
    options: &[String],
    config: &Config,
) -> Result<(BackgroundSession, Arc<MessageChannel>), MountProblem> {
    let mount_path = MountPath::reserve(&mountpoint)?;
    let api = Api::load().map_err(MountProblem::failed)?;
    let channel = unsafe { (api.create)() };
    if channel.is_null() {
        return Err(MountProblem::failed(
            "Could not create macFUSE's FSKit message channel",
        ));
    }
    let channel = Arc::new(MessageChannel {
        api,
        channel,
        mountpoint,
        lifecycle: Mutex::default(),
        _mount_path: Some(mount_path),
    });
    let options = CString::new(options.join(","))
        .map_err(|_| MountProblem::failed("mount options contain a NUL"))?;
    let deadline = Instant::now() + MOUNT_TIMEOUT;
    let (mount_tx, mount_rx) = mpsc::channel();
    let mounting = channel.clone();
    std::thread::Builder::new()
        .name("fskit-mount".into())
        .spawn(move || {
            let result = mounting.mount(&options);
            if mount_tx.send(result).is_err() {
                mounting.cancel();
            }
        })
        .map_err(|e| MountProblem::failed(e.to_string()))?;

    // FSKit needs INIT replies before MFMount can finish. Serve concurrently.
    let (session_tx, session_rx) = mpsc::channel();
    let serving = channel.clone();
    let mut config = config.clone();
    // FSKit sends startup STATFS requests as root, even for a user-owned mount.
    // Keep unrelated users denied while allowing the system's mount service.
    if config.acl == SessionACL::Owner {
        config.acl = SessionACL::RootAndOwner;
    }
    if let Err(error) = std::thread::Builder::new()
        .name("fskit-handshake".into())
        .spawn(move || {
            let result = Session::from_transport(fs, serving.clone(), config.acl, config)
                .and_then(|session| session.spawn());
            if session_tx.send(result).is_err() {
                serving.cancel();
            }
        })
    {
        channel.cancel();
        return Err(MountProblem::failed(error.to_string()));
    }
    let result = (|| {
        mount_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| {
                "FSKit did not finish mounting before its initialization deadline".to_string()
            })??;
        session_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| {
                "FSKit did not finish its FUSE handshake before its initialization deadline"
                    .to_string()
            })?
            .map_err(|error| error.to_string())
    })();
    match result {
        Ok(session) => Ok((session, channel)),
        Err(error) => {
            channel.cancel();
            Err(MountProblem::failed(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        parts: [libc::iovec; 2],
        released: AtomicUsize,
        closed: AtomicUsize,
        sent: Mutex<Vec<Vec<u8>>>,
    }

    unsafe extern "C" fn create() -> Ref {
        std::ptr::null_mut()
    }
    unsafe extern "C" fn mount(_: Ref, _: *const c_char, _: *const c_char, _: bool) -> c_int {
        0
    }
    unsafe extern "C" fn receive(channel: Ref) -> Ref {
        channel
    }
    unsafe extern "C" fn body(message: Ref, buffers: *mut *const libc::iovec) -> isize {
        let fake = unsafe { &*message.cast::<Fake>() };
        unsafe {
            *buffers = fake.parts.as_ptr();
        }
        2
    }
    unsafe extern "C" fn send(channel: Ref, buffers: *const libc::iovec, count: usize) -> isize {
        let fake = unsafe { &*channel.cast::<Fake>() };
        let parts = unsafe { std::slice::from_raw_parts(buffers, count) };
        let bytes: Vec<u8> = parts
            .iter()
            .flat_map(|part| {
                unsafe { std::slice::from_raw_parts(part.iov_base.cast::<u8>(), part.iov_len) }
                    .iter()
                    .copied()
            })
            .collect();
        let len = bytes.len();
        fake.sent.lock().unwrap().push(bytes);
        len as isize
    }
    unsafe extern "C" fn close(channel: Ref) -> bool {
        unsafe { &*channel.cast::<Fake>() }
            .closed
            .fetch_add(1, Ordering::SeqCst);
        true
    }
    unsafe extern "C" fn release(reference: Ref) {
        unsafe { &*reference.cast::<Fake>() }
            .released
            .fetch_add(1, Ordering::SeqCst);
    }
    static API: Api = Api {
        create,
        mount,
        receive,
        body,
        send,
        close,
        release,
    };

    fn fake() -> Box<Fake> {
        Box::new(Fake {
            parts: [
                libc::iovec {
                    iov_base: b"header".as_ptr().cast_mut().cast(),
                    iov_len: 6,
                },
                libc::iovec {
                    iov_base: b"payload".as_ptr().cast_mut().cast(),
                    iov_len: 7,
                },
            ],
            released: AtomicUsize::new(0),
            closed: AtomicUsize::new(0),
            sent: Mutex::default(),
        })
    }

    // Fake storage outlives the channel and stands in for retained framework objects.
    fn channel(fake: &mut Fake) -> MessageChannel {
        MessageChannel {
            api: &API,
            channel: (fake as *mut Fake).cast(),
            mountpoint: CString::new("/Volumes/FakeChannelTest").unwrap(),
            lifecycle: Mutex::default(),
            _mount_path: None,
        }
    }

    #[test]
    fn split_message_bodies_are_flattened_and_released_even_when_too_large() {
        let mut fake = fake();
        let channel = channel(&mut fake);
        let mut bytes = [0; 13];
        assert_eq!(channel.receive(&mut bytes).unwrap(), 13);
        assert_eq!(&bytes, b"headerpayload");
        assert_eq!(
            channel.receive(&mut [0; 12]).unwrap_err().raw_os_error(),
            Some(libc::EMSGSIZE)
        );
        assert_eq!(fake.released.load(Ordering::SeqCst), 2);
        channel
            .send(&[IoSlice::new(b"reply"), IoSlice::new(b"payload")])
            .unwrap();
        assert_eq!(*fake.sent.lock().unwrap(), vec![b"replypayload".to_vec()]);
        channel.close();
        channel.close();
        drop(channel);
        assert_eq!(fake.closed.load(Ordering::SeqCst), 1);
        assert_eq!(fake.released.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn cancelling_an_in_progress_mount_closes_its_channel_only_once() {
        let mut fake = fake();
        let channel = channel(&mut fake);
        channel.cancel();
        assert!(channel.lifecycle.lock().unwrap().cancelled);
        assert_eq!(fake.closed.load(Ordering::SeqCst), 1);
        // A completed failed MFMount already closed the channel itself.
        {
            let mut state = channel.lifecycle.lock().unwrap();
            state.mount_finished = true;
            state.closed = true;
        }
        drop(channel);
        assert_eq!(fake.closed.load(Ordering::SeqCst), 1);
        assert_eq!(fake.released.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn a_mount_path_cannot_be_reused_until_its_owner_finishes_cleanup() {
        let path = CString::new("/Volumes/ReservedMessageChannelTest").unwrap();
        let owner = MountPath::reserve(&path).unwrap();
        assert!(MountPath::reserve(&path).is_err());
        drop(owner);
        assert!(MountPath::reserve(&path).is_ok());
    }
    #[test]
    #[ignore = "requires macFUSE's installed MFMount framework"]
    fn the_installed_framework_exposes_the_message_api() {
        Api::load().unwrap();
    }
}
