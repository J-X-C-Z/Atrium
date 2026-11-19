//! Windows containment: no agent instruction runs before Job assignment.
//! Requires no breakaway flags; failures never fall back to an uncontained spawn.
use anyhow::{Context, Result, bail};
use std::{
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::null,
};
use tokio::process::{Child, Command};
use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, GetLastError, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, GetProcessIdOfThread, OpenThread, ResumeThread,
            THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME, TerminateProcess,
        },
    },
};

pub(super) struct Job(OwnedHandle);
impl Job {
    fn new() -> Result<Self> {
        // Null SECURITY_ATTRIBUTES means non-inheritable; unnamed avoids opening
        // or sharing an existing job controlled by another process.
        let raw = unsafe { CreateJobObjectW(null(), null()) };
        if raw.is_null() {
            return Err(std::io::Error::last_os_error()).context("create process job");
        }
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error()).context("configure process job");
        }
        Ok(job)
    }
    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle()
    }
}

fn primary_thread(pid: u32) -> Result<OwnedHandle> {
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error()).context("snapshot suspended process thread");
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    let mut thread_id = None;
    while more != 0 {
        if entry.th32OwnerProcessID == pid && thread_id.replace(entry.th32ThreadID).is_some() {
            bail!("unexpected additional threads in suspended process");
        }
        entry.dwSize = size_of::<THREADENTRY32>() as u32;
        more = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    if unsafe { GetLastError() } != ERROR_NO_MORE_FILES {
        return Err(std::io::Error::last_os_error()).context("enumerate suspended process thread");
    }
    let thread_id = thread_id.context("suspended process primary thread missing")?;
    let raw = unsafe {
        OpenThread(
            THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION,
            0,
            thread_id,
        )
    };
    if raw.is_null() {
        return Err(std::io::Error::last_os_error()).context("open owned primary thread");
    }
    let thread = unsafe { OwnedHandle::from_raw_handle(raw) };
    // PID remains reserved by the owned suspended process handle, and the opened
    // thread handle is stable even if the numeric thread ID could be recycled.
    if unsafe { GetProcessIdOfThread(thread.as_raw_handle()) } != pid {
        bail!("suspended thread ownership changed");
    }
    Ok(thread)
}

pub(super) fn spawn(cmd: &mut Command) -> Result<(Child, Job)> {
    let job = Job::new()?;
    cmd.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
        .kill_on_drop(true);
    let child = cmd.spawn().context("spawn suspended agent")?;
    let process = child.raw_handle().context("owned process handle missing")?;
    let setup = (|| -> Result<()> {
        if unsafe { AssignProcessToJobObject(job.raw(), process) } == 0 {
            return Err(std::io::Error::last_os_error()).context("assign suspended agent to job");
        }
        let thread = primary_thread(child.id().context("suspended process id missing")?)?;
        // Exactly one suspension was requested by CREATE_SUSPENDED. A different
        // count means external interference: fail closed instead of guessing.
        let previous_count = unsafe { ResumeThread(thread.as_raw_handle()) };
        if previous_count == u32::MAX {
            return Err(std::io::Error::last_os_error()).context("resume assigned agent");
        }
        if previous_count != 1 {
            bail!("unexpected primary thread suspend count");
        }
        Ok(())
    })();
    if let Err(error) = setup {
        // Covers assignment failure as well as later errors: only our retained
        // process handle is used, never OpenProcess on a possibly recycled PID.
        unsafe {
            TerminateProcess(process, 1);
        }
        return Err(error);
    }
    Ok((child, job))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;

    #[test]
    #[ignore = "fixture invoked only by owned_job_is_assigned_and_close_terminates"]
    fn job_child_fixture() {
        if std::env::var_os("ATRIUM_JOB_TEST_FIXTURE").is_some() {
            // Finite fallback even if the parent test fails unexpectedly.
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
    }

    #[tokio::test]
    async fn owned_job_is_assigned_and_close_terminates() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "procs::windows_process::tests::job_child_fixture",
            ])
            .env("ATRIUM_JOB_TEST_FIXTURE", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let (mut child, job) = spawn(&mut command).unwrap();
        let mut member = 0;
        let queried =
            unsafe { IsProcessInJob(child.raw_handle().unwrap(), job.raw(), &mut member) };
        // Dropping our non-inherited last Job handle must terminate the fixture.
        drop(job);
        let ended = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
        assert_ne!(queried, 0);
        assert_ne!(member, 0);
        assert!(!ended.unwrap().unwrap().success());
    }
}
