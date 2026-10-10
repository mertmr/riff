use std::{
    io,
    ops::{Deref, DerefMut},
    process::{Child, Command},
};

// Playback and extraction may spawn helpers of their own. Cancellation must
// reap the whole owned process tree, not leave a Python/JS extractor running.
pub(super) struct ChildProcess {
    child: Child,
    group_stopped: bool,
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
}
impl ChildProcess {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        }
        let child = command.spawn()?;
        #[cfg(windows)]
        let mut child = child;
        #[cfg(windows)]
        let job = {
            use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
            use windows_sys::Win32::System::JobObjects::*;
            let result = (|| {
                // A job handle stays owned until the child is reaped. Closing it
                // also kills descendants if shutdown unwinds unexpectedly.
                let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
                if raw.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let job = unsafe { OwnedHandle::from_raw_handle(raw) };
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION =
                    unsafe { std::mem::zeroed() };
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if unsafe {
                    SetInformationJobObject(
                        raw,
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        std::mem::size_of_val(&limits) as u32,
                    )
                } == 0
                    || unsafe { AssignProcessToJobObject(raw, child.as_raw_handle()) } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(job)
            })();
            match result {
                Ok(job) => job,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            }
        };
        Ok(Self {
            child,
            group_stopped: false,
            #[cfg(windows)]
            job,
        })
    }
    pub fn kill(&mut self) -> io::Result<()> {
        if !self.group_stopped {
            #[cfg(unix)]
            // The child was put in its own process group before exec.
            unsafe {
                libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
            }
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                unsafe {
                    windows_sys::Win32::System::JobObjects::TerminateJobObject(
                        self.job.as_raw_handle(),
                        1,
                    );
                }
            }
            self.group_stopped = true;
        }
        self.child.kill()
    }
}
impl Deref for ChildProcess {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}
impl DerefMut for ChildProcess {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}
impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        process::Stdio,
        time::{Duration, Instant},
    };
    #[test]
    fn cancellation_reaps_parent_and_stops_extractor_descendants() {
        let mut process = ChildProcess::spawn(
            Command::new("sh")
                .args(["-c", "sleep 30 & printf '%s\\n' \"$!\"; wait"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap();
        let mut pid = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut pid)
            .unwrap();
        let descendant: i32 = pid.trim().parse().unwrap();
        let parent = process.id() as i32;
        assert_eq!(unsafe { libc::getpgid(descendant) }, parent);
        drop(process);
        assert_eq!(unsafe { libc::kill(parent, 0) }, -1);
        let started = Instant::now();
        loop {
            let status = Command::new("ps")
                .args(["-o", "stat=", "-p", pid.trim()])
                .output()
                .unwrap();
            let status = String::from_utf8_lossy(&status.stdout);
            if status.trim().is_empty() || status.trim().starts_with('Z') {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "extractor descendant survived cancellation"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
