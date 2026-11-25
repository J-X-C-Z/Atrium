//! Contain each local launch and its descendants until the owner is dropped.
use anyhow::Result;
use std::process::{Child, Command, ExitStatus};

#[cfg(windows)]
pub mod windows;

#[cfg(unix)]
pub fn probe_owned_exit(pid: u32) -> std::io::Result<bool> {
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            return Ok(unsafe { info.si_pid() } == pid as libc::pid_t);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn unsupported() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "process trees are unsupported on this platform",
    )
}

pub struct ProcessTree {
    child: Child,
    stopped: bool,
    #[cfg(windows)]
    job: Option<windows::Job>,
}

impl ProcessTree {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            Ok(Self {
                child: command.process_group(0).spawn()?,
                stopped: false,
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::{io::AsHandle, process::CommandExt};
            let job = windows::Job::new()?;
            let mut child = command.creation_flags(windows::SPAWN_FLAGS).spawn()?;
            if let Err(error) = job.assign_and_resume(child.as_handle(), child.id()) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok(Self {
                child,
                stopped: false,
                job: Some(job),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = command;
            Err(unsupported().into())
        }
    }

    /// Keep the Unix leader's PID reserved until its process group is stopped.
    pub fn exited(&mut self) -> std::io::Result<bool> {
        #[cfg(unix)]
        {
            probe_owned_exit(self.child.id())
        }
        #[cfg(windows)]
        {
            self.child.try_wait().map(|status| status.is_some())
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(unsupported())
        }
    }

    fn stop(&mut self) {
        if std::mem::replace(&mut self.stopped, true) {
            return;
        }
        #[cfg(unix)]
        if probe_owned_exit(self.child.id()).is_ok() {
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        drop(self.job.take());
    }

    pub fn finish(mut self) -> std::io::Result<ExitStatus> {
        self.stop();
        self.child.wait()
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.stop();
        let _ = self.child.wait();
    }
}
