//! Owns only the authenticated daemon process launched by a test.
use rovik_client::local_backend::Stream;

#[cfg(unix)]
pub struct Host(i32);

#[cfg(unix)]
impl Host {
    pub fn peer(stream: &Stream) -> Self {
        Self(stream.peer_cred().unwrap().pid().unwrap())
    }

    pub fn crash(self) {
        assert_eq!(unsafe { libc::kill(self.0, libc::SIGKILL) }, 0);
        std::mem::forget(self);
    }
}

#[cfg(unix)]
impl Drop for Host {
    fn drop(&mut self) {
        unsafe { libc::kill(self.0, libc::SIGTERM) };
    }
}

#[cfg(windows)]
pub struct Host(std::os::windows::io::OwnedHandle);

#[cfg(windows)]
impl Host {
    pub fn peer(stream: &Stream) -> Self {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows::Win32::{
            Foundation::HANDLE,
            System::{
                Pipes::GetNamedPipeServerProcessId,
                Threading::{OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE},
            },
        };
        let Stream::Client(pipe) = stream else {
            panic!("expected a client pipe");
        };
        let mut pid = 0;
        unsafe {
            GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut pid).unwrap();
            // Retain the process handle so cleanup cannot target a reused PID.
            let handle = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid).unwrap();
            Self(std::os::windows::io::OwnedHandle::from_raw_handle(handle.0))
        }
    }

    pub fn crash(self) {
        self.terminate().unwrap();
    }

    fn terminate(&self) -> windows::core::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::{HANDLE, WAIT_OBJECT_0},
            System::Threading::{TerminateProcess, WaitForSingleObject},
        };
        unsafe {
            let handle = HANDLE(self.0.as_raw_handle());
            if WaitForSingleObject(handle, 0) != WAIT_OBJECT_0 {
                TerminateProcess(handle, 1)?;
                assert_eq!(WaitForSingleObject(handle, 10_000), WAIT_OBJECT_0);
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}
