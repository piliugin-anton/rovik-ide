//! Retained OS process identities for explicitly stopping legacy backends.
use super::Stream;
use std::io;

#[cfg(target_os = "linux")]
pub(super) struct Owner(std::os::fd::OwnedFd);

#[cfg(target_os = "linux")]
impl Owner {
    pub fn capture(stream: &Stream) -> io::Result<Option<Self>> {
        use std::os::fd::FromRawFd;
        let pid = stream
            .peer_cred()?
            .pid()
            .ok_or_else(|| io::Error::other("Backend process identity unavailable"))?;
        if pid <= 0 || pid as u32 == std::process::id() {
            return Err(io::Error::other("Refusing to stop the application process"));
        }
        // A pidfd pins the process, including after exit; PID reuse cannot redirect a signal.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(Self(unsafe {
            std::os::fd::OwnedFd::from_raw_fd(fd as i32)
        })))
    }
    pub fn terminate(&self, force: bool) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.0.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } < 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
    pub fn exited(&self) -> io::Result<bool> {
        use std::os::fd::AsRawFd;
        let mut fd = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut fd, 1, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fd.revents & libc::POLLIN != 0)
    }
}

#[cfg(windows)]
pub(super) struct Owner(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
impl Owner {
    pub fn capture(stream: &Stream) -> io::Result<Option<Self>> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows::Win32::{
            Foundation::HANDLE,
            System::{
                Pipes::GetNamedPipeServerProcessId,
                Threading::{
                    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
                    PROCESS_TERMINATE,
                },
            },
        };
        let Stream::Client(pipe) = stream else {
            return Err(io::Error::other("Expected backend client pipe"));
        };
        let mut pid = 0;
        unsafe { GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut pid) }
            .map_err(io::Error::other)?;
        if pid == 0 || pid == std::process::id() {
            return Err(io::Error::other("Refusing to stop the application process"));
        }
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                false,
                pid,
            )
        }
        .map_err(io::Error::other)?;
        let owned = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        super::platform::check_owner(HANDLE(owned.as_raw_handle()))?;
        Ok(Some(Self(owned)))
    }
    pub fn terminate(&self, _force: bool) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{Foundation::HANDLE, System::Threading::TerminateProcess};
        if !self.exited()? {
            unsafe { TerminateProcess(HANDLE(self.0.as_raw_handle()), 1) }
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
    pub fn exited(&self) -> io::Result<bool> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::{HANDLE, WAIT_FAILED, WAIT_OBJECT_0},
            System::Threading::WaitForSingleObject,
        };
        let status = unsafe { WaitForSingleObject(HANDLE(self.0.as_raw_handle()), 0) };
        if status == WAIT_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(status == WAIT_OBJECT_0)
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub(super) struct Owner;
#[cfg(not(any(target_os = "linux", windows)))]
impl Owner {
    pub fn capture(_stream: &Stream) -> io::Result<Option<Self>> {
        Ok(None)
    }
    pub fn terminate(&self, _force: bool) -> io::Result<()> {
        Err(io::Error::other("This older backend cannot be stopped safely; finish its work and close older Rovik windows, then retry"))
    }
    pub fn exited(&self) -> io::Result<bool> {
        Ok(true)
    }
}
