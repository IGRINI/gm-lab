//! Cross-platform child-process helpers: `CREATE_NO_WINDOW` on Windows, and
//! process-tree kill (Windows Job Object with a `taskkill /T` fallback; Unix
//! process group via `setpgid` + `killpg`). PORT_PLAN §3.2 sidecar row / risk #7.

use tokio::process::Command;

/// Apply the platform "no console window" flag and group the child so its whole
/// process tree can be killed later.
///
/// - **Windows**: `CREATE_NO_WINDOW` so spawned Python/ffmpeg children don't
///   pop a console. (Job-Object assignment happens post-spawn in
///   [`ProcessTree::attach`].)
/// - **Unix**: put the child in its own process group (`setpgid(0,0)` in a
///   `pre_exec`) so the whole tree can be signalled via `killpg`.
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        // CREATE_NO_WINDOW = 0x08000000.
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        // SAFETY: setpgid(0, 0) only adjusts the calling (child) process group;
        // it is async-signal-safe and valid in the pre_exec context.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

/// A handle that owns the means to kill a spawned child's entire process tree.
///
/// On Windows we create a Job Object, assign the child to it, and configure
/// `KILL_ON_JOB_CLOSE` so dropping/closing the job terminates the tree (plus an
/// explicit `TerminateJobObject`, with a `taskkill /T /F /PID` fallback). On
/// Unix we remember the process-group id and `killpg(SIGKILL)`.
pub struct ProcessTree {
    pid: u32,
    #[cfg(windows)]
    job: Option<WinJob>,
}

#[cfg(windows)]
struct WinJob(windows_sys::Win32::Foundation::HANDLE);

// HANDLE is a raw pointer; the job handle is owned and only touched from this
// type, so it is safe to move across threads.
#[cfg(windows)]
unsafe impl Send for WinJob {}
#[cfg(windows)]
unsafe impl Sync for WinJob {}

impl ProcessTree {
    /// Attach to an already-spawned child. `pid` is the OS process id.
    ///
    /// On Windows this creates a Job Object and assigns the child to it; on
    /// failure it falls back to plain PID kill / `taskkill`.
    pub fn attach(pid: u32) -> Self {
        #[cfg(windows)]
        {
            let job = unsafe { create_kill_on_close_job(pid) };
            ProcessTree { pid, job }
        }
        #[cfg(not(windows))]
        {
            ProcessTree { pid }
        }
    }

    /// Kill the child and its whole process tree. Best-effort.
    pub fn kill(&mut self) {
        #[cfg(windows)]
        {
            let mut killed = false;
            if let Some(job) = &self.job {
                unsafe {
                    use windows_sys::Win32::System::JobObjects::TerminateJobObject;
                    if TerminateJobObject(job.0, 1) != 0 {
                        killed = true;
                    }
                }
            }
            if !killed {
                // Fallback: taskkill /T (tree) /F (force).
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &self.pid.to_string(), "/T", "/F"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }
        #[cfg(unix)]
        {
            // Kill the whole process group (negative pid == group). The child
            // was made a group leader via setpgid in `no_window`.
            unsafe {
                libc::killpg(self.pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            if let Some(job) = self.job.take() {
                // Closing a KILL_ON_JOB_CLOSE job terminates the tree.
                unsafe {
                    windows_sys::Win32::Foundation::CloseHandle(job.0);
                }
            }
        }
    }
}

#[cfg(windows)]
unsafe fn create_kill_on_close_job(pid: u32) -> Option<WinJob> {
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    let job: HANDLE = CreateJobObjectW(std::ptr::null(), std::ptr::null());
    if job.is_null() {
        return None;
    }

    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let ok = SetInformationJobObject(
        job,
        JobObjectExtendedLimitInformation,
        &info as *const _ as *const core::ffi::c_void,
        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
    );
    if ok == 0 {
        windows_sys::Win32::Foundation::CloseHandle(job);
        return None;
    }

    let proc = OpenProcess(PROCESS_TERMINATE | PROCESS_SET_QUOTA, 0, pid);
    if proc.is_null() {
        windows_sys::Win32::Foundation::CloseHandle(job);
        return None;
    }
    let assigned = AssignProcessToJobObject(job, proc);
    windows_sys::Win32::Foundation::CloseHandle(proc);
    if assigned == 0 {
        windows_sys::Win32::Foundation::CloseHandle(job);
        return None;
    }
    Some(WinJob(job))
}
