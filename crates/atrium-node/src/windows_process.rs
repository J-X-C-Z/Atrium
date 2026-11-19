use anyhow::{Context, Result};
pub(super) use atrium_process::windows::Job;
use std::os::windows::io::BorrowedHandle;
use tokio::process::{Child, Command};

pub(super) fn spawn(cmd: &mut Command) -> Result<(Child, Job)> {
    let job = Job::new()?;
    cmd.creation_flags(atrium_process::windows::SPAWN_FLAGS)
        .kill_on_drop(true);
    let mut child = cmd.spawn().context("spawn suspended agent")?;
    let handle = child.raw_handle().context("owned process handle missing")?;
    let pid = child.id().context("suspended process id missing")?;
    if let Err(error) = job.assign_and_resume(unsafe { BorrowedHandle::borrow_raw(handle) }, pid) {
        let _ = child.start_kill();
        return Err(error);
    }
    Ok((child, job))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use std::{process::Stdio, time::Duration};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    use windows_sys::Win32::System::JobObjects::IsProcessInJob;

    #[test]
    #[ignore = "fixture invoked only by owned_job_is_assigned_and_close_terminates"]
    fn job_child_fixture() {
        match std::env::var("ATRIUM_JOB_TEST_FIXTURE").as_deref() {
            Ok("child") => {
                let mut grandchild = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(FIXTURE_ARGS)
                    .env("ATRIUM_JOB_TEST_FIXTURE", "grandchild")
                    .spawn()
                    .unwrap();
                grandchild.wait().unwrap();
            }
            Ok("grandchild") => {
                use std::io::Write;
                println!("grandchild-ready");
                std::io::stdout().flush().unwrap();
                // Finite fallback even if job termination fails.
                std::thread::sleep(Duration::from_secs(25));
            }
            _ => {}
        }
    }

    const FIXTURE_ARGS: [&str; 4] = [
        "--ignored",
        "--exact",
        "procs::windows_process::tests::job_child_fixture",
        "--nocapture",
    ];

    #[tokio::test]
    async fn owned_job_is_assigned_and_close_terminates() {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(FIXTURE_ARGS)
            .env("ATRIUM_JOB_TEST_FIXTURE", "child")
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        let (mut child, job) = spawn(&mut command).unwrap();
        let mut member = 0;
        let queried = unsafe {
            IsProcessInJob(
                child.raw_handle().unwrap(),
                std::os::windows::io::AsHandle::as_handle(&job).as_raw_handle(),
                &mut member,
            )
        };
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let ready = tokio::time::timeout(Duration::from_secs(10), async {
            let mut line = String::new();
            while stdout.read_line(&mut line).await.unwrap() != 0 {
                if line.contains("grandchild-ready") {
                    return true;
                }
                line.clear();
            }
            false
        })
        .await;
        // EOF requires both the direct child and its grandchild to close stdout.
        drop(job);
        let (ended, eof) = tokio::time::timeout(Duration::from_secs(10), async {
            let mut trailing = Vec::new();
            tokio::join!(child.wait(), stdout.read_to_end(&mut trailing))
        })
        .await
        .unwrap();
        assert!(ready.unwrap());
        assert_ne!(queried, 0);
        assert_ne!(member, 0);
        assert!(!ended.unwrap().success());
        eof.unwrap();
    }
}
