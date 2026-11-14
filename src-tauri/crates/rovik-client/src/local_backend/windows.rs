//! Private, local-only named pipes. Pipe handles arbitrate ownership without
//! relying on replaceable lock files or thread-affine Windows mutexes.
use sha2::{Digest, Sha256};
use std::{
    io,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions},
    sync::Mutex,
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{LocalFree, HANDLE, HLOCAL},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            },
            GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
            TOKEN_USER,
        },
        Storage::FileSystem::{
            FileIdInfo, GetFileInformationByHandleEx, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_ID_INFO, PIPE_ACCESS_DUPLEX,
        },
        System::{
            Pipes::{
                CreateNamedPipeW, GetNamedPipeClientProcessId, GetNamedPipeServerProcessId,
                PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
            },
            Threading::{
                GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    },
};

fn win(error: windows::core::Error) -> io::Error {
    io::Error::from_raw_os_error(error.code().0 & 0xffff)
}

struct LocalMemory(*mut std::ffi::c_void);
impl Drop for LocalMemory {
    fn drop(&mut self) {
        unsafe {
            LocalFree(Some(HLOCAL(self.0)));
        }
    }
}

fn user(process: HANDLE) -> io::Result<String> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) }.map_err(win)?;
    // Successful OpenProcessToken transfers one owned handle to us.
    let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let mut length = 0;
    let handle = HANDLE(token.as_raw_handle());
    let result = unsafe { GetTokenInformation(handle, TokenUser, None, 0, &mut length) };
    if length == 0 {
        return Err(result
            .err()
            .map(win)
            .unwrap_or_else(|| io::Error::other("Token user is empty")));
    }
    // Pointer alignment is required for TOKEN_USER, not just byte alignment.
    let mut buffer = vec![0usize; (length as usize).div_ceil(std::mem::size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            handle,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            length,
            &mut length,
        )
    }
    .map_err(win)?;
    let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut sid = PWSTR::null();
    unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut sid) }.map_err(win)?;
    let _memory = LocalMemory(sid.0.cast());
    unsafe { sid.to_string() }.map_err(|error| io::Error::other(error.to_string()))
}

fn current_user() -> io::Result<String> {
    user(unsafe { GetCurrentProcess() })
}

pub(super) fn check_owner(process: HANDLE) -> io::Result<()> {
    if user(process)? != current_user()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Backend peer belongs to another user",
        ));
    }
    Ok(())
}

fn check_peer(handle: HANDLE, server: bool) -> io::Result<()> {
    let mut pid = 0;
    unsafe {
        if server {
            GetNamedPipeClientProcessId(handle, &mut pid)
        } else {
            GetNamedPipeServerProcessId(handle, &mut pid)
        }
    }
    .map_err(win)?;
    let process =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.map_err(win)?;
    let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
    if user(HANDLE(process.as_raw_handle()))? != current_user()? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Backend peer belongs to another user",
        ));
    }
    Ok(())
}

fn security() -> io::Result<LocalMemory> {
    // Protected DACL: no inherited access for other local users. The owner
    // alone may connect or create additional instances of these pipes.
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})", current_user()?)
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            1,
            &mut descriptor,
            None,
        )
    }
    .map_err(win)?;
    Ok(LocalMemory(descriptor.0))
}

fn attributes(memory: &LocalMemory) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: memory.0,
        bInheritHandle: false.into(),
    }
}

fn name(directory: &Path, purpose: &str) -> io::Result<String> {
    use std::os::windows::fs::OpenOptionsExt;
    // File identity unifies path casing, junctions and directory aliases without
    // lossy string normalization (Windows also supports case-sensitive folders).
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(directory)?;
    // Use the 128-bit identity so ReFS directories cannot collide through
    // the legacy API's truncated file index.
    let mut info = FILE_ID_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(directory.as_raw_handle()),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .map_err(win)?;
    let mut hash = Sha256::new();
    hash.update(current_user()?.as_bytes());
    hash.update(info.VolumeSerialNumber.to_le_bytes());
    hash.update(info.FileId.Identifier);
    let digest: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!(r"\\.\pipe\rovik-desktop-{digest}-{purpose}"))
}

/// Exclusive ownership retained independently of any UI or connected client.
pub struct Lease {
    _handle: OwnedHandle,
    directory: PathBuf,
}
impl Lease {
    /// Acquires ownership before initializing any services.
    pub fn acquire(directory: &Path) -> io::Result<Option<Self>> {
        Self::acquire_named(directory, "owner.lock")
    }
    pub(super) fn acquire_named(directory: &Path, purpose: &str) -> io::Result<Option<Self>> {
        std::fs::create_dir_all(directory)?;
        let pipe: Vec<u16> = name(directory, purpose)?
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let descriptor = security()?;
        let attrs = attributes(&descriptor);
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(pipe.as_ptr()),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                0,
                0,
                Some(&attrs),
            )
        };
        if handle.is_invalid() {
            let error = io::Error::last_os_error();
            // Windows reports ACCESS_DENIED or PIPE_BUSY while an owner holds
            // its single instance. Never steal ownership, even from an unknown peer.
            if matches!(error.raw_os_error(), Some(5 | 231)) {
                return Ok(None);
            }
            return Err(error);
        }
        Ok(Some(Self {
            _handle: unsafe { OwnedHandle::from_raw_handle(handle.0) },
            directory: directory.into(),
        }))
    }
    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }
}

/// Bidirectional authenticated local transport used by the common RPC layer.
#[derive(Debug)]
pub enum Stream {
    /// An accepted connection owned by the daemon.
    Server(NamedPipeServer),
    /// A UI connection to the daemon.
    Client(NamedPipeClient),
}
impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(pipe) => Pin::new(pipe).poll_read(cx, buf),
            Self::Client(pipe) => Pin::new(pipe).poll_read(cx, buf),
        }
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Server(pipe) => Pin::new(pipe).poll_write(cx, buf),
            Self::Client(pipe) => Pin::new(pipe).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(pipe) => Pin::new(pipe).poll_flush(cx),
            Self::Client(pipe) => Pin::new(pipe).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Server(pipe) => Pin::new(pipe).poll_shutdown(cx),
            Self::Client(pipe) => Pin::new(pipe).poll_shutdown(cx),
        }
    }
}

pub(super) struct Endpoint {
    name: String,
    pending: Mutex<NamedPipeServer>,
}
fn pipe(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    let descriptor = security()?;
    let mut attrs = attributes(&descriptor);
    // The descriptor and SECURITY_ATTRIBUTES remain alive through CreateNamedPipe.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attrs as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}
impl Endpoint {
    pub fn bind(directory: &Path, purpose: &str) -> io::Result<Self> {
        let name = name(directory, purpose)?;
        Ok(Self {
            pending: Mutex::new(pipe(&name, true)?),
            name,
        })
    }
    pub async fn accept(&self) -> io::Result<Stream> {
        let mut pending = self.pending.lock().await;
        // connect() is cancellation-safe. Keep the waiting instance in self,
        // and publish its successor before yielding the accepted connection.
        pending.connect().await?;
        let stream = std::mem::replace(&mut *pending, pipe(&self.name, false)?);
        check_peer(HANDLE(stream.as_raw_handle()), true)?;
        Ok(Stream::Server(stream))
    }
}

pub(super) async fn connect_endpoint(directory: &Path, purpose: &str) -> io::Result<Stream> {
    let name = name(directory, purpose)?;
    loop {
        match ClientOptions::new().open(&name) {
            Ok(stream) => {
                check_peer(HANDLE(stream.as_raw_handle()), false)?;
                return Ok(Stream::Client(stream));
            }
            // An existing listener is accepting another client. This is never
            // evidence that a new backend should be started; caller bounds wait.
            Err(error) if error.raw_os_error() == Some(231) => {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directory_aliases_share_identity_but_other_directories_do_not() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        assert_eq!(
            name(first.path(), "rpc").unwrap(),
            name(&first.path().join("."), "rpc").unwrap()
        );
        assert_ne!(
            name(first.path(), "rpc").unwrap(),
            name(second.path(), "rpc").unwrap()
        );
        assert_ne!(
            name(first.path(), "rpc").unwrap(),
            name(first.path(), "control").unwrap()
        );
    }

    #[tokio::test]
    async fn published_pipes_grant_access_only_to_the_current_user() {
        use windows::Win32::Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo,
                SE_KERNEL_OBJECT,
            },
            GetAce, ACCESS_ALLOWED_ACE, DACL_SECURITY_INFORMATION, PSID,
        };
        let root = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(root.path(), "rpc").unwrap();
        let pending = endpoint.pending.lock().await;
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let mut dacl = std::ptr::null_mut();
        unsafe {
            GetSecurityInfo(
                HANDLE(pending.as_raw_handle()),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(&mut dacl),
                None,
                Some(&mut descriptor),
            )
        }
        .ok()
        .unwrap();
        let _descriptor = LocalMemory(descriptor.0);
        let mut sddl = PWSTR::null();
        unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                1,
                DACL_SECURITY_INFORMATION,
                &mut sddl,
                None,
            )
        }
        .unwrap();
        let _sddl = LocalMemory(sddl.0.cast());
        let text = unsafe { sddl.to_string() }.unwrap();
        assert!(
            text.starts_with("D:P"),
            "DACL must forbid inheritance: {text}"
        );
        assert_eq!(
            text.matches('(').count(),
            1,
            "only one access entry: {text}"
        );
        assert!(text.contains("(A;"), "the entry must allow access: {text}");
        // SDDL abbreviates well-known accounts (including a renamed local
        // administrator) as LA/SY/etc. Compare the ACE's actual SID instead.
        assert!(!dacl.is_null(), "the pipe must have an explicit DACL");
        assert_eq!(unsafe { (*dacl).AceCount }, 1);
        let mut ace = std::ptr::null_mut();
        unsafe { GetAce(dacl, 0, &mut ace) }.unwrap();
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let trustee = PSID(std::ptr::addr_of!(allowed.SidStart).cast_mut().cast());
        let mut sid = PWSTR::null();
        unsafe { ConvertSidToStringSidW(trustee, &mut sid) }.unwrap();
        let _sid = LocalMemory(sid.0.cast());
        assert_eq!(
            unsafe { sid.to_string() }.unwrap(),
            current_user().unwrap(),
            "only the current user may receive access: {text}"
        );
    }

    #[tokio::test]
    async fn windows_share_one_owner_with_private_sessions_and_reacquire_after_shutdown() {
        let root = tempfile::tempdir().unwrap();
        let lease = Lease::acquire(root.path()).unwrap().unwrap();
        assert!(Lease::acquire(root.path()).unwrap().is_none());
        let listener = lease.listen("test").unwrap();
        let mut ready = Vec::new();
        for _ in 0..2 {
            let (server, client) =
                tokio::join!(listener.accept(), super::super::connect(root.path()));
            let mut server = server.unwrap();
            let (mut client, identity) = client.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            client.write_u32(42).await.unwrap();
            assert_eq!(server.read_u32().await.unwrap(), 42);
            ready.push(identity);
        }
        assert_eq!(ready[0].boot, ready[1].boot);
        assert_ne!(ready[0].session, ready[1].session);
        drop(listener);
        assert!(Lease::acquire(root.path()).unwrap().is_none());
        drop(lease);
        assert!(Lease::acquire(root.path()).unwrap().is_some());
    }
    #[tokio::test]
    async fn a_second_endpoint_cannot_preempt_a_live_listener() {
        let root = tempfile::tempdir().unwrap();
        let _first = Endpoint::bind(root.path(), "rpc").unwrap();
        assert!(Endpoint::bind(root.path(), "rpc").is_err());
    }
    #[tokio::test]
    async fn cancelling_accept_keeps_the_listener_available() {
        let root = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(root.path(), "rpc").unwrap();
        let mut accept = Box::pin(endpoint.accept());
        assert!(futures_util::poll!(&mut accept).is_pending());
        drop(accept);
        let (server, client) =
            tokio::join!(endpoint.accept(), connect_endpoint(root.path(), "rpc"));
        server.unwrap();
        client.unwrap();
    }
    #[tokio::test]
    async fn a_failed_windows_launcher_is_reported_without_the_startup_timeout() {
        let root = tempfile::tempdir().unwrap();
        let mut command = rovik_core::process::command("cmd.exe");
        command.args(["/D", "/C", "exit", "7"]);
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::super::ensure_command(command, root.path(), root.path()),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.to_string().contains("exited with"), "{error}");
    }

    #[tokio::test]
    async fn existing_backend_is_attached_without_spawning_a_launcher() {
        let root = tempfile::tempdir().unwrap();
        let directory = super::super::directory(root.path());
        let lease = Lease::acquire(&directory).unwrap().unwrap();
        let listener = lease.listen("test").unwrap();
        let command = std::process::Command::new("missing-launcher-must-not-run.exe");
        let (server, client) = tokio::join!(
            listener.accept(),
            super::super::ensure_command(command, root.path(), root.path())
        );
        server.unwrap();
        client.unwrap();
    }
}
