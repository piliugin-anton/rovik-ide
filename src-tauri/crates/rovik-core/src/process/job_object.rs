// src-tauri/crates/rovik-core/src/process/job_object.rs
//
// A Windows job object: a process put in one as it starts, and everything
// it starts, even once their parent has exited, can be ended at once. The
// agent's commands are in one (command_tool::ProcessTree), so is a
// terminal's shell (process/session_tree.rs).

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    QueryInformationJobObject, TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
};

/// A job object holding a started process, and so all it starts.
pub(crate) struct JobObject(HANDLE);

// A job's handle may be used, and closed, from any thread.
unsafe impl Send for JobObject {}
unsafe impl Sync for JobObject {}

impl JobObject {
    /// A new job holding the process `process` (a handle to it, with the
    /// rights to set its quota and end it); None where it can't be made or
    /// the process can't go in it (before Windows 8, with the app in a job
    /// of its own already).
    pub(crate) fn of(process: HANDLE) -> Option<JobObject> {
        // SAFETY: a new unnamed job, whose handle Drop closes once; a handle
        // that isn't a process's only makes the assignment fail.
        unsafe {
            let job = JobObject(CreateJobObjectW(None, PCWSTR::null()).ok()?);
            AssignProcessToJobObject(job.0, process).ok()?;
            Some(job)
        }
    }

    /// Ends every process in the job: whether it could.
    pub(crate) fn kill(&self) -> bool {
        // SAFETY: the handle is open until Drop.
        unsafe { TerminateJobObject(self.0, 1).is_ok() }
    }

    /// Whether no process of the job is still running. False when Windows
    /// can't say.
    pub(crate) fn is_empty(&self) -> bool {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        // SAFETY: the handle is open until Drop, and `info` is the struct
        // this class of information fills, of its size.
        let asked = unsafe {
            QueryInformationJobObject(
                Some(self.0),
                JobObjectBasicAccountingInformation,
                std::ptr::from_mut(&mut info).cast(),
                std::mem::size_of_val(&info) as u32,
                None,
            )
        };
        asked.is_ok() && info.ActiveProcesses == 0
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        // Closing it ends nothing: what's left in it is ended by its owner
        // when it should be (`ProcessTree::end_leftovers`,
        // `SessionTree::end`).
        // SAFETY: opened in `of`, closed only here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
