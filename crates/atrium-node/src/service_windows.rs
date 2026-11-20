use super::{path_text, xml};
#[cfg(windows)]
use anyhow::Context;
use anyhow::{Result, bail};
use std::path::Path;

/// Quote one Windows CommandLineToArgvW argument without a command shell.
fn windows_arg(s: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in s.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        if c == '"' {
            out.push_str(&"\\".repeat(slashes * 2 + 1));
            out.push('"');
        } else {
            out.push_str(&"\\".repeat(slashes));
            out.push(c);
        }
        slashes = 0;
    }
    out.push_str(&"\\".repeat(slashes * 2));
    out.push('"');
    out
}
fn render_windows(exe: &Path, config: &Path) -> Result<String> {
    let action_path = |p: &Path| -> Result<String> {
        let path = path_text(p)?;
        // Task Scheduler expands environment references in Exec actions.
        if path.contains('%') {
            bail!("scheduled task paths must not contain percent signs");
        }
        // Task Scheduler expects ordinary Win32 paths in its Exec action.
        if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
            Ok(format!(r"\\{unc}"))
        } else {
            Ok(path.strip_prefix(r"\\?\").unwrap_or(path).to_owned())
        }
    };
    let e = xml(&action_path(exe)?);
    let c = xml(&windows_arg(&action_path(config)?));
    // RegisterTask receives a UTF-16 BSTR, so leave byte encoding unspecified.
    Ok(format!(
        r#"<Task version="1.3" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
<RegistrationInfo><Description>Atrium per-user agent node (managed v2)</Description></RegistrationInfo>
<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>ATRIUM_USER_SID</UserId></LogonTrigger></Triggers>
<Principals><Principal id="Author"><UserId>ATRIUM_USER_SID</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
<Settings><RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><Enabled>true</Enabled></Settings>
<Actions Context="Author"><Exec><Command>{e}</Command><Arguments>serve --config {c}</Arguments></Exec></Actions>
</Task>
"#
    ))
}

// ITaskFolder::RegisterTask with TASK_CREATE does not overwrite an existing task.
// Compare the registered description and principal before stopping or deleting.
#[cfg(windows)]
const TASK_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$name = 'AtriumNode-' + $sid
$service = New-Object -ComObject Schedule.Service
$service.Connect()
$folder = $service.GetFolder('\')
$action = $env:ATRIUM_TASK_ACTION
if ($action -eq 'install' -or $action -eq 'validate') {
    [xml] $definition = $env:ATRIUM_TASK_XML
    $definition.Task.Triggers.LogonTrigger.UserId = $sid
    $definition.Task.Principals.Principal.UserId = $sid
    if ($action -eq 'validate') {
        $null = $folder.RegisterTask($name, $definition.OuterXml, 1, $sid, $null, 3, $null)
        return
    }
    $task = $folder.RegisterTask($name, $definition.OuterXml, 2, $sid, $null, 3, $null)
    try {
        $null = $task.Run($null)
    } catch {
        try { $folder.DeleteTask($name, 0) } catch { [Console]::Error.WriteLine($_) }
        throw
    }
    return
}
try {
    $task = $folder.GetTask($name)
} catch {
    # COM can wrap the HRESULT in a PowerShell invocation exception.
    $errorCode = $_.Exception.GetBaseException().HResult
    if ($action -eq 'uninstall' -and $errorCode -eq -2147024894) { return }
    throw
}
if ($task.Definition.RegistrationInfo.Description -ne 'Atrium per-user agent node (managed v2)') {
    throw ('Refusing to operate on an unmanaged task: ' + $name)
}
$principal = $task.Definition.Principal.UserId
if ($principal -ne $sid -and ([Security.Principal.NTAccount]::new($principal)).Translate([Security.Principal.SecurityIdentifier]).Value -ne $sid) {
    throw ('Refusing to operate on a task owned by a different principal: ' + $name)
}
if ($action -eq 'uninstall') {
    if ($task.State -eq 2 -or $task.State -eq 4) { $task.Stop(0) }
    $folder.DeleteTask($name, 0)
} else {
    $task | Select-Object Name, State, LastRunTime, LastTaskResult | Format-List
}
"#;

#[cfg(windows)]
fn task(action: &str, definition: &str) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    let system_root = std::env::var_os("SystemRoot").context("SystemRoot unavailable")?;
    let powershell =
        Path::new(&system_root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    path_text(&powershell)?;
    let status = std::process::Command::new(&powershell)
        .args(["-NoProfile", "-NonInteractive", "-Command", TASK_SCRIPT])
        .env("ATRIUM_TASK_ACTION", action)
        .env("ATRIUM_TASK_XML", definition)
        .stdin(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .context("run Windows Task Scheduler command")?;
    if !status.success() {
        bail!("Task Scheduler {action} failed with {status}");
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn run(action: &str, config: &Path) -> Result<()> {
    if action == "install" {
        let config = config
            .canonicalize()
            .context("create a valid config before installing")?;
        let exe = std::env::current_exe()?.canonicalize()?;
        return task(action, &render_windows(&exe, &config)?);
    }
    task(action, "")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn windows_scheduler_accepts_definition_without_registration() {
        let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
        let config = std::env::temp_dir().join("settings space & 日本語.toml");
        task("validate", &render_windows(&exe, &config).unwrap()).unwrap();
    }

    #[test]
    fn windows_quote_backslashes_and_quotes() {
        assert_eq!(windows_arg("C:\\a b\\"), "\"C:\\a b\\\\\"");
        assert_eq!(windows_arg("a\"b"), "\"a\\\"b\"");
    }
    #[test]
    fn windows_task_is_unprivileged_and_escapes_arguments() {
        // Use host-native absolute paths: this tests XML and command-line
        // generation, not Windows task registration on another OS.
        let base = std::env::temp_dir();
        let exe = base.join("node & helper.exe");
        let config = base.join("settings space & quote.toml");
        let output = render_windows(&exe, &config).unwrap();
        assert!(output.contains("node &amp; helper.exe</Command>"));
        assert_eq!(
            output.matches("<UserId>ATRIUM_USER_SID</UserId>").count(),
            2
        );
        assert!(output.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(output.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(output.contains("<Arguments>serve --config &quot;"));
        assert!(output.contains("settings space &amp; quote.toml&quot;</Arguments>"));
        assert!(!output.contains("cmd.exe"));
        assert!(!output.contains("HighestAvailable"));
    }
}
