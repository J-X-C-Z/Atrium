//! Windows containment: no agent instruction runs before Job assignment.
//! Requires no breakaway flags; failures never fall back to an uncontained spawn.
use anyhow::{Context, Result, bail};
use std::os::windows::io::BorrowedHandle;
use std::{
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::null,
};
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
            GetProcessIdOfThread, OpenThread, ResumeThread, THREAD_QUERY_LIMITED_INFORMATION,
            THREAD_SUSPEND_RESUME,
        },
    },
};

pub struct Job(OwnedHandle);
impl Job {
    pub fn new() -> Result<Self> {
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

pub const SPAWN_FLAGS: u32 = windows_sys::Win32::System::Threading::CREATE_SUSPENDED
    | windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

impl Job {
    pub fn assign_and_resume(&self, process: BorrowedHandle<'_>, pid: u32) -> Result<()> {
        if unsafe { AssignProcessToJobObject(self.raw(), process.as_raw_handle()) } == 0 {
            return Err(std::io::Error::last_os_error()).context("assign suspended agent to job");
        }
        let thread = primary_thread(pid)?;
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
    }
}

impl std::os::windows::io::AsHandle for Job {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.0.as_handle()
    }
}
