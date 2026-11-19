//! Bounded, reconnectable process transport. Agents' payloads remain opaque.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::{Mutex, mpsc, watch},
    task::JoinHandle,
};

#[cfg(windows)]
#[path = "windows_process.rs"]
mod windows_process;

pub const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;
const REPLAY_PAGE_BYTES: usize = 256 * 1024;
const INPUT_BUDGET: usize = 128 * 1024 * 1024;
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// Only retained output is budgeted here. Reader buffers, replay copies and
// transport allocations are transient and are not an RSS hard limit.
#[derive(Default)]
struct OutputStore {
    rings: HashMap<usize, RingData>,
    next_id: usize,
    bytes: usize,
    max_bytes: usize,
}
struct RingData {
    frames: VecDeque<(i64, Arc<Value>, usize)>,
    next: i64,
    bytes: usize,
    max_lines: usize,
    max_bytes: usize,
    // Fixed-size terminal metadata must survive output eviction, including -1.
    terminal: Option<(i64, Arc<Value>)>,
}
impl RingData {
    fn evict(&mut self) -> usize {
        let size = self.frames.pop_front().map_or(0, |f| f.2);
        self.bytes -= size;
        size
    }
}
struct Ring {
    id: usize,
    store: Arc<StdMutex<OutputStore>>,
}
impl Ring {
    #[cfg(test)]
    fn new(lines: usize, bytes: usize) -> Self {
        Self::with_limits(
            lines,
            bytes,
            Arc::new(StdMutex::new(OutputStore {
                max_bytes: bytes,
                ..OutputStore::default()
            })),
        )
    }
    fn shared(lines: usize, bytes: usize, store: Arc<StdMutex<OutputStore>>) -> Self {
        // Preserve the legacy effective per-process minimum for max-size frames.
        Self::with_limits(lines, bytes.max(MAX_LINE_BYTES + 1024), store)
    }
    fn with_limits(lines: usize, bytes: usize, store: Arc<StdMutex<OutputStore>>) -> Self {
        let mut output = store.lock().unwrap();
        let id = output.next_id;
        output.next_id += 1;
        output.rings.insert(
            id,
            RingData {
                frames: VecDeque::new(),
                next: 1,
                bytes: 0,
                max_lines: lines.max(1),
                max_bytes: bytes,
                terminal: None,
            },
        );
        drop(output);
        Self { id, store }
    }
    fn push(&mut self, mut value: Value) {
        // Callers may hold process state; this lock never acquires process state.
        let mut output = self.store.lock().unwrap();
        let ring = output.rings.get_mut(&self.id).unwrap();
        let seq = ring.next;
        ring.next += 1;
        value["seq"] = json!(seq);
        if value.get("exit").is_some() {
            ring.terminal = Some((seq, Arc::new(value)));
            return;
        }
        let size = value
            .get("line")
            .or_else(|| value.get("stderr"))
            .and_then(Value::as_str)
            .map_or(128, |s| s.len() + 128);
        ring.bytes += size;
        ring.frames.push_back((seq, Arc::new(value), size));
        let mut removed = 0;
        while ring.frames.len() > ring.max_lines || ring.bytes > ring.max_bytes {
            removed += ring.evict();
        }
        output.bytes += size;
        output.bytes -= removed;
        while output.bytes > output.max_bytes {
            // Reclaim the largest consumer first, oldest frame first. Stable ID
            // breaks ties deterministically. A noisy producer cannot reserve the
            // whole budget forever while other processes have no history.
            let victim = *output
                .rings
                .iter()
                .filter(|(_, r)| r.bytes > 0)
                .max_by_key(|(id, r)| (r.bytes, std::cmp::Reverse(**id)))
                .unwrap()
                .0;
            let released = output.rings.get_mut(&victim).unwrap().evict();
            output.bytes -= released;
        }
    }
    fn snapshot(&self, after: i64, ended: bool) -> Snapshot {
        let output = self.store.lock().unwrap();
        let ring = &output.rings[&self.id];
        let latest = ring.next - 1;
        let first = ring
            .frames
            .front()
            .map(|f| f.0)
            .or_else(|| ring.terminal.as_ref().map(|f| f.0))
            .unwrap_or(ring.next);
        let skip_backlog = after == -1;
        let after = if skip_backlog { latest } else { after.max(0) };
        let gap = (after < first - 1).then(|| json!({"gap":{"from":after + 1,"to":first - 1}}));
        let mut frames = Vec::new();
        let mut bytes = 0usize;
        let mut more = false;
        for (_, frame, size) in ring.frames.iter().filter(|f| f.0 > after) {
            // Always permit one legal large frame, otherwise a 64 MiB frame
            // could never make progress through the smaller replay page.
            if !frames.is_empty() && bytes.saturating_add(*size) > REPLAY_PAGE_BYTES {
                more = true;
                break;
            }
            bytes = bytes.saturating_add(*size);
            frames.push(frame.clone());
        }
        if !more
            && let Some((seq, terminal)) = &ring.terminal
            && (*seq > after || (skip_backlog && ended))
        {
            if !frames.is_empty() && bytes.saturating_add(128) > REPLAY_PAGE_BYTES {
                more = true;
            } else {
                frames.push(terminal.clone());
            }
        }
        // Capture cheap references while locked, then clone potentially large
        // protocol payloads outside the shared store lock.
        drop(output);
        Snapshot {
            gap,
            frames: frames.into_iter().map(|frame| (*frame).clone()).collect(),
            latest,
            ended: ended && !more,
            more,
        }
    }
}
impl Drop for Ring {
    fn drop(&mut self) {
        let mut output = self.store.lock().unwrap();
        if let Some(ring) = output.rings.remove(&self.id) {
            output.bytes -= ring.bytes;
        }
    }
}

pub struct Snapshot {
    pub gap: Option<Value>,
    pub frames: Vec<Value>,
    pub latest: i64,
    /// True only after this page includes the final retained backlog/exit.
    pub ended: bool,
    /// Another page must be fetched immediately, without waiting for new output.
    pub more: bool,
}
struct State {
    ring: Ring,
    exit: Option<Value>,
    generation: u64,
    attached: bool,
    detached_at: Instant,
    ended_at: Option<Instant>,
}
struct Input {
    line: String,
    budget: Arc<AtomicUsize>,
}
impl Drop for Input {
    fn drop(&mut self) {
        self.budget.fetch_sub(self.line.len(), Ordering::AcqRel);
    }
}

pub struct Process {
    pub id: String,
    agent: String,
    cwd: PathBuf,
    owner: String,
    pid: u32,
    started_at: u64,
    state: Mutex<State>,
    changed: watch::Sender<u64>,
    input: mpsc::Sender<Input>,
    input_bytes: Arc<AtomicUsize>,
    stop: watch::Sender<bool>,
    #[cfg(windows)]
    job: windows_process::Job,
}
impl Process {
    async fn push(&self, value: Value) {
        self.state.lock().await.ring.push(value);
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }
    pub async fn json(&self) -> Value {
        let state = self.state.lock().await;
        json!({"id":self.id,"agent":self.agent,"cwd":self.cwd,"owner":self.owner,"pid":self.pid,"started_at":self.started_at,"attached":state.attached,"exit":state.exit})
    }
    pub async fn attach(&self) -> (u64, watch::Receiver<u64>) {
        let rx = self.changed.subscribe();
        let mut state = self.state.lock().await;
        state.generation = state.generation.wrapping_add(1);
        state.attached = true;
        let generation = state.generation;
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
        (generation, rx)
    }
    pub async fn detach(&self, generation: u64) {
        let mut state = self.state.lock().await;
        if state.generation == generation && state.attached {
            state.attached = false;
            state.detached_at = Instant::now();
        }
    }
    pub async fn is_current(&self, generation: u64) -> bool {
        self.state.lock().await.generation == generation
    }
    pub async fn snapshot(&self, after: i64) -> Snapshot {
        let s = self.state.lock().await;
        s.ring.snapshot(after, s.exit.is_some())
    }
    /// Serialize input admission against attachment replacement.
    pub async fn try_input_for(&self, generation: u64, line: String) -> Result<()> {
        let state = self.state.lock().await;
        if !state.attached || state.generation != generation {
            bail!("attachment replaced or detached");
        }
        self.try_input(line)
    }
    pub fn try_input(&self, line: String) -> Result<()> {
        if line.len() > MAX_LINE_BYTES || line.contains('\n') {
            bail!("invalid or oversized input line");
        }
        if *self.stop.borrow() {
            bail!("process is stopping");
        }
        let size = line.len();
        // Explicit CAS keeps compatibility with the workspace MSRV and newer
        // Rust releases that deprecated AtomicUsize::fetch_update.
        let mut queued = self.input_bytes.load(Ordering::Acquire);
        loop {
            let Some(next) = queued.checked_add(size).filter(|n| *n <= INPUT_BUDGET) else {
                bail!("stdin byte queue full");
            };
            match self.input_bytes.compare_exchange_weak(
                queued,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => queued = actual,
            }
        }
        let input = Input {
            line,
            budget: self.input_bytes.clone(),
        };
        self.input
            .try_send(input)
            .map_err(|_| anyhow::anyhow!("stdin queue full or closed"))
    }
    pub fn request_stop(&self) {
        self.stop.send_replace(true);
    }
    pub async fn wait_ended(&self) {
        let mut rx = self.changed.subscribe();
        loop {
            if self.state.lock().await.exit.is_some() {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

#[derive(Default)]
struct Registry {
    processes: HashMap<String, Arc<Process>>,
    closing: bool,
}

pub struct ProcessManager {
    registry: Mutex<Registry>,
    keep_lines: usize,
    keep_bytes: usize,
    output: Arc<StdMutex<OutputStore>>,
    timeout: Duration,
}
impl ProcessManager {
    #[cfg(test)]
    pub fn new(keep_lines: usize, keep_bytes: usize, timeout_secs: u64) -> Self {
        Self::with_total_output_bytes(keep_lines, keep_bytes, timeout_secs, keep_bytes)
    }
    pub fn with_total_output_bytes(
        keep_lines: usize,
        keep_bytes: usize,
        timeout_secs: u64,
        total_bytes: usize,
    ) -> Self {
        Self {
            registry: Mutex::new(Registry::default()),
            keep_lines,
            keep_bytes,
            output: Arc::new(StdMutex::new(OutputStore {
                max_bytes: total_bytes,
                ..OutputStore::default()
            })),
            timeout: Duration::from_secs(timeout_secs.max(1)),
        }
    }
    pub async fn spawn(
        &self,
        agent: String,
        command: Vec<String>,
        cwd: PathBuf,
        owner: String,
        env: HashMap<String, String>,
        path: String,
    ) -> Result<Arc<Process>> {
        if command.is_empty() {
            bail!("empty command");
        }
        let mut registry = self.registry.lock().await;
        if registry.closing {
            bail!("process manager is shutting down");
        }
        let processes = &mut registry.processes;
        // Bound even authenticated clients: each process may hold large protocol frames.
        if processes.len() >= 128 {
            bail!("process capacity reached");
        }
        let mut cmd = Command::new(&command[0]);
        cmd.args(&command[1..])
            .current_dir(&cwd)
            .env("PATH", path)
            .envs(env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(false);
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(windows)]
        let (mut child, job) = windows_process::spawn(&mut cmd)?;
        #[cfg(not(windows))]
        let mut child = cmd.spawn().context("spawn agent")?;
        let pid = child.id().context("child pid")?;
        let stdin = child.stdin.take().context("child stdin")?;
        let stdout = child.stdout.take().context("child stdout")?;
        let stderr = child.stderr.take().context("child stderr")?;
        let (changed, _) = watch::channel(0);
        let (stop, stop_rx) = watch::channel(false);
        let (input, input_rx) = mpsc::channel(32);
        let proc = Arc::new(Process {
            id: format!("p-{}", uuid::Uuid::new_v4()),
            agent,
            cwd,
            owner,
            pid,
            started_at: now(),
            state: Mutex::new(State {
                ring: Ring::shared(self.keep_lines, self.keep_bytes, self.output.clone()),
                exit: None,
                generation: 0,
                attached: false,
                detached_at: Instant::now(),
                ended_at: None,
            }),
            changed,
            input,
            input_bytes: Arc::new(AtomicUsize::new(0)),
            stop,
            #[cfg(windows)]
            job,
        });
        processes.insert(proc.id.clone(), proc.clone());
        drop(registry);
        let writer = tokio::spawn(write_input(stdin, input_rx, stop_rx.clone()));
        let out = tokio::spawn(read_output(stdout, proc.clone(), false));
        let err = tokio::spawn(read_output(stderr, proc.clone(), true));
        tokio::spawn(reap(child, proc.clone(), stop_rx, writer, out, err));
        Ok(proc)
    }
    pub async fn get(&self, id: &str) -> Option<Arc<Process>> {
        self.registry.lock().await.processes.get(id).cloned()
    }
    pub async fn list(&self, owner: Option<&str>) -> Vec<Value> {
        let procs: Vec<_> = self
            .registry
            .lock()
            .await
            .processes
            .values()
            .filter(|p| owner.is_none_or(|o| p.owner == o))
            .cloned()
            .collect();
        let mut rows = Vec::with_capacity(procs.len());
        for p in procs {
            rows.push(p.json().await);
        }
        rows
    }
    pub async fn stop(&self, id: &str) -> Result<()> {
        let p = self.get(id).await.context("not_found")?;
        p.request_stop();
        p.wait_ended().await;
        Ok(())
    }
    pub async fn shutdown(&self) {
        let ps: Vec<_> = {
            let mut registry = self.registry.lock().await;
            // The same lock gates spawning through insertion: every accepted child
            // is included before shutdown can close admission.
            registry.closing = true;
            registry.processes.values().cloned().collect()
        };
        for p in &ps {
            p.request_stop();
        }
        for p in ps {
            p.wait_ended().await;
        }
    }
    pub async fn maintenance(&self) {
        let ps: Vec<_> = self
            .registry
            .lock()
            .await
            .processes
            .values()
            .cloned()
            .collect();
        let mut expired = Vec::new();
        for p in ps {
            let s = p.state.lock().await;
            if s.ended_at.is_some_and(|at| at.elapsed() >= self.timeout) {
                expired.push(p.id.clone());
            } else if s.exit.is_none() && !s.attached && s.detached_at.elapsed() >= self.timeout {
                p.request_stop();
            }
        }
        let mut registry = self.registry.lock().await;
        for id in expired {
            registry.processes.remove(&id);
        }
    }
}

async fn write_input(
    mut stdin: tokio::process::ChildStdin,
    mut rx: mpsc::Receiver<Input>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        tokio::select! { biased;
            _=stop.changed()=>break,
            input=rx.recv()=> { let Some(input)=input else {break};
                tokio::select! { biased; _=stop.changed()=>break,
                    result=async { stdin.write_all(input.line.as_bytes()).await?; stdin.write_all(b"\n").await?; stdin.flush().await } => {if result.is_err(){break;}}
                }
            }
        }
    }
    // Dropping stdin is deliberate: graceful EOF precedes any signals.
}

async fn read_output<R: AsyncRead + Unpin>(mut reader: R, p: Arc<Process>, stderr: bool) {
    let mut chunk = [0u8; 8192];
    let mut line = Vec::new();
    loop {
        let n = match reader.read(&mut chunk).await {
            Ok(n) => n,
            Err(e) => {
                p.push(json!({"stderr":format!("node: output read failed: {e}")}))
                    .await;
                break;
            }
        };
        if n == 0 {
            if !line.is_empty() {
                emit_line(&p, &line, stderr).await;
            }
            break;
        }
        for byte in &chunk[..n] {
            if *byte == b'\n' {
                emit_line(&p, &line, stderr).await;
                line.clear();
            } else {
                if line.len() == MAX_LINE_BYTES {
                    p.push(json!({"stderr":"node: output line exceeds 64 MiB; stopping process"}))
                        .await;
                    p.request_stop();
                    return;
                }
                line.push(*byte);
            }
        }
    }
}
async fn emit_line(p: &Process, line: &[u8], stderr: bool) {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let text = String::from_utf8_lossy(line);
    if text.len() > MAX_LINE_BYTES {
        p.push(json!({"stderr":"node: decoded output line exceeds 64 MiB; stopping process"}))
            .await;
        p.request_stop();
        return;
    }
    p.push(if stderr {
        json!({"stderr":text})
    } else {
        json!({"line":text})
    })
    .await;
}
async fn reap(
    mut child: Child,
    p: Arc<Process>,
    mut stop: watch::Receiver<bool>,
    writer: JoinHandle<()>,
    mut out: JoinHandle<()>,
    mut err: JoinHandle<()>,
) {
    #[cfg(unix)]
    let status = wait_owned_group(&mut child, p.pid, &mut stop).await;
    #[cfg(windows)]
    let status = tokio::select! {
        status = child.wait() => status,
        _ = async { if !*stop.borrow() { let _ = stop.changed().await; } } => {
            // The writer observes the same stop signal and drops stdin. Allow
            // cooperative EOF shutdown before terminating the Windows job.
            match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
                Ok(status) => status,
                Err(_) => match p.job.terminate() {
                    Ok(()) => child.wait().await,
                    Err(error) => Err(error),
                },
            }
        }
    };
    #[cfg(windows)]
    if let Err(error) = p.job.terminate() {
        p.push(json!({"stderr":format!("node: job cleanup failed: {error}")}))
            .await;
    }
    writer.abort();
    let _ = writer.await;
    // A descendant can keep a pipe open after the direct child exits. Bound
    // drainage rather than inventing a PID-based signal after the child is reaped.
    let mut out_joined = false;
    let mut err_joined = false;
    let drained = tokio::time::timeout(Duration::from_secs(2), async {
        let _ = (&mut out).await;
        out_joined = true;
        let _ = (&mut err).await;
        err_joined = true;
    })
    .await;
    if drained.is_err() {
        out.abort();
        err.abort();
        if !out_joined {
            let _ = out.await;
        }
        if !err_joined {
            let _ = err.await;
        }
        p.push(json!({"stderr":"node: inherited output pipe did not close; trailing output unavailable"})).await;
    }
    let (code, signal) = match status {
        Ok(s) => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                (
                    s.code(),
                    s.signal().map(|v| match v {
                        libc::SIGTERM => "SIGTERM".to_string(),
                        libc::SIGKILL => "SIGKILL".to_string(),
                        _ => format!("SIG{v}"),
                    }),
                )
            }
            #[cfg(not(unix))]
            {
                (s.code(), None::<String>)
            }
        }
        Err(e) => {
            p.push(json!({"stderr":format!("node: wait failed: {e}")}))
                .await;
            (None, None)
        }
    };
    let mut s = p.state.lock().await;
    s.exit = Some(json!({"code":code,"signal":signal,"at":now()}));
    s.ended_at = Some(Instant::now());
    s.ring.push(json!({"exit":{"code":code,"signal":signal}}));
    drop(s);
    p.changed.send_modify(|v| *v = v.wrapping_add(1));
}
/// Observe exit without reaping: the reserved PID prevents PGID reuse until
/// the last group signal. Child stays exclusively owned here. In the locked
/// Tokio version, signal/pidfd reapers call try_wait only when polled or dropped;
/// its orphan queue only contains dropped children. Do not poll Child::wait,
/// try_wait, or drop it before group cleanup; re-audit on Tokio upgrades.
/// This also requires no external waitpid(-1) reaper or SIGCHLD=SIG_IGN /
/// SA_NOCLDWAIT handler in the embedding process: those break child ownership.
#[cfg(unix)]
async fn exited_unreaped(pid: u32) -> std::io::Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut changes = signal(SignalKind::child())?;
    loop {
        if probe_owned_exit(pid)? {
            return Ok(());
        }
        changes.recv().await;
    }
}

#[cfg(unix)]
fn probe_owned_exit(pid: u32) -> std::io::Result<bool> {
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

#[cfg(unix)]
async fn wait_owned_group(
    child: &mut Child,
    pid: u32,
    stop: &mut watch::Receiver<bool>,
) -> std::io::Result<std::process::ExitStatus> {
    let observed = tokio::select! {
        result = exited_unreaped(pid) => result,
        _ = async { if !*stop.borrow() { let _ = stop.changed().await; } } => {
            match tokio::time::timeout(Duration::from_secs(2), exited_unreaped(pid)).await {
                Ok(result) => result,
                Err(_) => {
                    signal_owned(child, pid, false);
                    match tokio::time::timeout(Duration::from_secs(5), exited_unreaped(pid)).await {
                        Ok(result) => result,
                        Err(_) => {
                            signal_owned(child, pid, true);
                            exited_unreaped(pid).await
                        }
                    }
                }
            }
        }
    };
    // Never send another group signal if observation failed (e.g. ECHILD):
    // ownership may have been broken by an external child reaper.
    observed?;
    // A leader's exit ends this node-owned group's lifetime. Give remaining
    // members a short TERM grace period, then terminate before reaping leader.
    // Descendants deliberately escaping with setsid/setpgid are NOT contained.
    signal_owned(child, pid, false);
    tokio::time::sleep(Duration::from_millis(100)).await;
    signal_owned(child, pid, true);
    child.wait().await
}

#[cfg(unix)]
fn signal_owned(child: &mut Child, pid: u32, kill: bool) {
    #[cfg(unix)]
    {
        // Called only while this unreaped child is owned by this reaper.
        if child.id() == Some(pid) && probe_owned_exit(pid).is_ok() {
            unsafe {
                libc::kill(
                    -(pid as i32),
                    if kill { libc::SIGKILL } else { libc::SIGTERM },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn shutdown_permanently_rejects_spawn() {
        let m = ProcessManager::new(10, 128 * 1024 * 1024, 60);
        m.shutdown().await;
        // No executable is invoked: rejection must precede Command::spawn.
        let result = m
            .spawn(
                "test".into(),
                vec!["must-not-be-executed".into()],
                std::env::temp_dir(),
                "owner".into(),
                HashMap::new(),
                String::new(),
            )
            .await;
        assert_eq!(
            result.err().unwrap().to_string(),
            "process manager is shutting down"
        );
        assert!(m.list(None).await.is_empty());
        m.shutdown().await;
        assert!(m.registry.lock().await.closing);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "owned EOF fixture, invoked only by windows_stop_allows_graceful_eof"]
    fn windows_eof_fixture() {
        if std::env::var_os("ATRIUM_EOF_TEST_FIXTURE").is_none() {
            return;
        }
        use std::io::{Read, Write};
        println!("ready-for-eof");
        std::io::stdout().flush().unwrap();
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input).unwrap();
        println!("received-eof");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_stop_allows_graceful_eof() {
        let m = ProcessManager::new(100, 128 * 1024 * 1024, 60);
        let p = m
            .spawn(
                "test".into(),
                vec![
                    std::env::current_exe().unwrap().to_str().unwrap().into(),
                    "--ignored".into(),
                    "--exact".into(),
                    "procs::tests::windows_eof_fixture".into(),
                    "--nocapture".into(),
                ],
                std::env::temp_dir(),
                "owner".into(),
                HashMap::from([("ATRIUM_EOF_TEST_FIXTURE".into(), "1".into())]),
                std::env::var("PATH").unwrap_or_default(),
            )
            .await
            .unwrap();
        let (_, mut changes) = p.attach().await;
        let ready = tokio::time::timeout(Duration::from_secs(5), async {
            while !p.snapshot(0).await.frames.iter().any(|v| {
                v["line"]
                    .as_str()
                    .is_some_and(|line| line.contains("ready-for-eof"))
            }) {
                changes.changed().await.unwrap();
            }
        })
        .await;
        let stopped = tokio::time::timeout(Duration::from_secs(5), m.stop(&p.id)).await;
        m.shutdown().await;
        assert!(ready.is_ok());
        stopped.unwrap().unwrap();
        assert_eq!(p.json().await["exit"]["code"], 0);
        assert!(p.snapshot(0).await.frames.iter().any(|v| {
            v["line"]
                .as_str()
                .is_some_and(|line| line.contains("received-eof"))
        }));
    }

    #[test]
    fn replay_pages_preserve_sequence_and_exit_without_new_output() {
        let mut r = Ring::new(10, 2 * 1024 * 1024);
        for _ in 0..5 {
            r.push(json!({"line":"x".repeat(100 * 1024)}));
        }
        r.push(json!({"exit":{"code":0}}));
        let mut after = 0;
        let mut seqs = Vec::new();
        let mut pages = 0;
        loop {
            let page = r.snapshot(after, true);
            pages += 1;
            assert_eq!(page.latest, 6); // Never use high-water as pagination cursor.
            assert_eq!(page.ended, !page.more);
            for frame in &page.frames {
                after = frame["seq"].as_i64().unwrap();
                seqs.push(after);
            }
            if !page.more {
                assert!(page.frames.last().unwrap().get("exit").is_some());
                break;
            }
            assert!(pages < 10);
        }
        assert_eq!(pages, 3);
        assert_eq!(seqs, vec![1, 2, 3, 4, 5, 6]);
        let tail = r.snapshot(-1, true);
        assert!(tail.ended && !tail.more && tail.gap.is_none());
        assert_eq!(tail.frames.len(), 1);
        assert_eq!(tail.frames[0]["seq"], 6);
    }

    #[test]
    fn replay_page_handles_eviction_between_pages_and_a_large_first_frame() {
        let mut r = Ring::new(3, 2 * 1024 * 1024);
        for _ in 0..4 {
            r.push(json!({"line":"x".repeat(300 * 1024)}));
        }
        let page = r.snapshot(0, false);
        assert_eq!(page.gap, Some(json!({"gap":{"from":1,"to":1}})));
        assert!(page.more && !page.ended);
        assert_eq!(page.frames.len(), 1); // first frame may exceed the page budget
        assert_eq!(page.frames[0]["seq"], 2);
        r.push(json!({"line":"a"}));
        r.push(json!({"line":"b"})); // sequence 3 is evicted between snapshots
        let next = r.snapshot(2, false);
        assert_eq!(next.gap, Some(json!({"gap":{"from":3,"to":3}})));
        assert_eq!(next.frames[0]["seq"], 4);
        assert!(next.more);
        let tail = r.snapshot(4, false);
        assert_eq!(
            tail.frames
                .iter()
                .map(|v| v["seq"].as_i64().unwrap())
                .collect::<Vec<_>>(),
            vec![5, 6]
        );
        assert!(!tail.more);
        assert!(r.snapshot(-1, false).frames.is_empty());
    }

    #[test]
    fn ring_reports_gap_and_order() {
        let mut r = Ring::new(2, 128 * 1024 * 1024);
        for i in 0..4 {
            r.push(json!({"line":i.to_string()}));
        }
        let s = r.snapshot(0, false);
        assert_eq!(s.gap, Some(json!({"gap":{"from":1,"to":2}})));
        assert_eq!(s.frames[0]["seq"], 3);
        assert_eq!(s.frames[1]["seq"], 4);
        assert_eq!(s.latest, 4);
        assert!(r.snapshot(-1, false).frames.is_empty());
    }
    #[test]
    fn ring_keeps_exit() {
        let mut r = Ring::new(1, 128 * 1024 * 1024);
        r.push(json!({"line":"a"}));
        r.push(json!({"exit":{"code":0,"signal":null}}));
        let s = r.snapshot(1, true);
        assert!(s.ended);
        assert_eq!(s.frames.len(), 1);
        assert!(s.frames[0].get("exit").is_some());
        assert_eq!(r.snapshot(-1, true).frames.len(), 1);
        assert!(r.snapshot(-1, true).frames[0].get("exit").is_some());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn owned_cat_roundtrip_and_graceful_eof() {
        let m = ProcessManager::new(100, 128 * 1024 * 1024, 60);
        let p = m
            .spawn(
                "test".into(),
                vec!["/bin/cat".into()],
                std::env::temp_dir(),
                "owner".into(),
                HashMap::new(),
                "/usr/bin:/bin".into(),
            )
            .await
            .unwrap();
        let (g, mut changes) = p.attach().await;
        // Oversized input must be rejected before queue admission or pipe writes.
        let error = p.try_input("x".repeat(MAX_LINE_BYTES + 1)).unwrap_err();
        assert_eq!(error.to_string(), "invalid or oversized input line");
        assert_eq!(p.input_bytes.load(Ordering::Acquire), 0);
        p.try_input("hello".into()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if !p.snapshot(0).await.frames.is_empty() {
                    break;
                }
                changes.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(p.snapshot(0).await.frames[0]["line"], "hello");
        let (g2, _) = p.attach().await;
        assert!(!p.is_current(g).await);
        assert!(p.try_input_for(g, "stale".into()).await.is_err());
        p.detach(g).await;
        assert_eq!(p.json().await["attached"], true);
        p.detach(g2).await;
        tokio::time::timeout(Duration::from_secs(5), m.stop(&p.id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(p.json().await["exit"]["code"], 0);
        assert!(
            p.snapshot(0)
                .await
                .frames
                .last()
                .unwrap()
                .get("exit")
                .is_some()
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn owned_process_ignoring_eof_is_terminated_after_grace_period() {
        let m = ProcessManager::new(100, 128 * 1024 * 1024, 60);
        let p = m
            .spawn(
                "test".into(),
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    // Only this owned fixture is signalled. cat consumes stdin until
                    // EOF; exec leaves no background descendant to clean up.
                    "printf 'ready\n'; /bin/cat >/dev/null; printf 'eof\n'; exec /bin/sleep 10"
                        .into(),
                ],
                std::env::temp_dir(),
                "owner".into(),
                HashMap::new(),
                "/usr/bin:/bin".into(),
            )
            .await
            .unwrap();
        let (_, mut changes) = p.attach().await;
        let ready = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if p.snapshot(0)
                    .await
                    .frames
                    .iter()
                    .any(|v| v["line"] == "ready")
                {
                    break;
                }
                changes.changed().await.unwrap();
            }
        })
        .await;
        let began = Instant::now();
        let stopped = tokio::time::timeout(Duration::from_secs(5), m.stop(&p.id)).await;
        let elapsed = began.elapsed();
        // Finish cleanup even if the timeout assertion will fail.
        m.shutdown().await;
        assert!(ready.is_ok(), "fixture did not become ready");
        stopped.expect("stop exceeded TERM grace window").unwrap();
        assert!(
            elapsed >= Duration::from_secs(2),
            "TERM sent before EOF grace period"
        );
        assert_eq!(p.json().await["exit"]["signal"], "SIGTERM");
        assert!(
            p.snapshot(0)
                .await
                .frames
                .iter()
                .any(|v| v["line"] == "eof")
        );
        // This does not claim coverage of descendants surviving a reaped leader,
        // nor of the longer SIGKILL escalation path.
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn owned_term_ignoring_process_escalates_to_kill() {
        let m = ProcessManager::new(100, 128 * 1024 * 1024, 60);
        let p = m
            .spawn(
                "test".into(),
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "trap '' TERM; printf 'ready\n'; exec /bin/sleep 15".into(),
                ],
                std::env::temp_dir(),
                "owner".into(),
                HashMap::new(),
                "/usr/bin:/bin".into(),
            )
            .await
            .unwrap();
        let (_, mut changes) = p.attach().await;
        let ready = tokio::time::timeout(Duration::from_secs(3), async {
            while !p
                .snapshot(0)
                .await
                .frames
                .iter()
                .any(|v| v["line"] == "ready")
            {
                changes.changed().await.unwrap();
            }
        })
        .await;
        let start = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(11), m.stop(&p.id)).await;
        let elapsed = start.elapsed();
        m.shutdown().await;
        assert!(ready.is_ok());
        result.unwrap().unwrap();
        assert!(elapsed >= Duration::from_secs(7));
        assert_eq!(p.json().await["exit"]["signal"], "SIGKILL");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn leader_exit_cleans_same_group_inherited_pipe() {
        let m = ProcessManager::new(100, 128 * 1024 * 1024, 60);
        let p = m
            .spawn(
                "test".into(),
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    // The background child inherits both the process group and pipes.
                    // Finite fallback lifetime prevents permanent leakage on test failure.
                    "(/bin/sleep 10) & printf 'leader\n'; exit 17".into(),
                ],
                std::env::temp_dir(),
                "owner".into(),
                HashMap::new(),
                "/usr/bin:/bin".into(),
            )
            .await
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), p.wait_ended()).await;
        m.shutdown().await;
        assert!(result.is_ok());
        assert_eq!(p.json().await["exit"]["code"], 17);
        let frames = p.snapshot(0).await.frames;
        assert!(frames.iter().any(|v| v["line"] == "leader"));
        // Reaching actual pipe EOF, rather than the bounded drain fallback,
        // verifies the inherited writer was terminated after its leader exited.
        assert!(!frames.iter().any(|v| v.get("stderr").is_some()));
    }

    #[test]
    fn byte_budget_evicts_without_losing_sequence() {
        let mut r = Ring::new(100, 270);
        r.push(json!({"line":"first"}));
        r.push(json!({"line":"second"}));
        r.push(json!({"line":"third"}));
        assert!(r.store.lock().unwrap().bytes <= 270);
        assert_eq!(
            r.snapshot(0, false).gap,
            Some(json!({"gap":{"from":1,"to":1}}))
        );
        assert_eq!(r.snapshot(0, false).frames.len(), 2);
    }
    #[test]
    fn legacy_minimum_retains_max_frame_accounting() {
        let store = Arc::new(StdMutex::new(OutputStore {
            max_bytes: MAX_LINE_BYTES + 128,
            ..OutputStore::default()
        }));
        let ring = Ring::shared(10, MAX_LINE_BYTES, store.clone());
        assert_eq!(
            store.lock().unwrap().rings[&ring.id].max_bytes,
            MAX_LINE_BYTES + 1024
        );
    }
    #[test]
    fn global_output_budget_reclaims_largest_consumer_and_releases_on_drop() {
        let store = Arc::new(StdMutex::new(OutputStore {
            max_bytes: 390,
            ..OutputStore::default()
        }));
        let mut noisy = Ring::with_limits(100, 1000, store.clone());
        let mut quiet = Ring::with_limits(100, 1000, store.clone());
        noisy.push(json!({"line":"a"}));
        noisy.push(json!({"line":"b"}));
        quiet.push(json!({"line":"q"}));
        for _ in 0..20 {
            noisy.push(json!({"line":"n"}));
            assert!(store.lock().unwrap().bytes <= 390);
        }
        assert_eq!(quiet.snapshot(0, false).frames[0]["line"], "q");
        let snapshot = noisy.snapshot(0, false);
        assert_eq!(snapshot.gap, Some(json!({"gap":{"from":1,"to":20}})));
        assert_eq!(snapshot.frames.len(), 2);
        drop(noisy);
        assert_eq!(store.lock().unwrap().bytes, 129);
        drop(quiet);
        assert_eq!(store.lock().unwrap().bytes, 0);
        assert!(store.lock().unwrap().rings.is_empty());
    }
    #[test]
    fn global_budget_and_per_process_budget_preserve_terminal_and_gap() {
        let store = Arc::new(StdMutex::new(OutputStore {
            max_bytes: 130,
            ..OutputStore::default()
        }));
        let mut a = Ring::with_limits(10, 130, store.clone());
        let mut b = Ring::with_limits(10, 1000, store.clone());
        a.push(json!({"line":"a"}));
        a.push(json!({"exit":{"code":0,"signal":null}}));
        b.push(json!({"line":"b"}));
        let ended = a.snapshot(0, true);
        assert_eq!(ended.gap, Some(json!({"gap":{"from":1,"to":1}})));
        assert_eq!(ended.frames.len(), 1);
        assert_eq!(ended.frames[0]["seq"], 2);
        assert!(a.snapshot(-1, true).frames[0].get("exit").is_some());
        // A frame larger than either limit is dropped, never retained above it.
        b.push(json!({"line":"long"}));
        assert!(b.snapshot(0, false).frames.is_empty());
        assert_eq!(
            b.snapshot(0, false).gap,
            Some(json!({"gap":{"from":1,"to":2}}))
        );
        assert_eq!(store.lock().unwrap().bytes, 0);
        let mut c = Ring::with_limits(10, 128, store.clone());
        c.push(json!({"line":"c"}));
        assert_eq!(store.lock().unwrap().bytes, 0);
        assert_eq!(c.snapshot(0, false).latest, 1);
    }
    #[test]
    fn dropping_input_releases_byte_budget() {
        let budget = Arc::new(AtomicUsize::new(4));
        let input = Input {
            line: "test".into(),
            budget: budget.clone(),
        };
        drop(input);
        assert_eq!(budget.load(Ordering::Acquire), 0);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn detached_timeout_stops_owned_process_and_retains_exit() {
        let m = ProcessManager::new(10, 128 * 1024 * 1024, 1);
        let p = m
            .spawn(
                "test".into(),
                vec!["/bin/cat".into()],
                std::env::temp_dir(),
                "owner".into(),
                HashMap::new(),
                "/usr/bin:/bin".into(),
            )
            .await
            .unwrap();
        {
            let mut state = p.state.lock().await;
            state.detached_at = Instant::now() - Duration::from_secs(2);
        }
        m.maintenance().await;
        tokio::time::timeout(Duration::from_secs(5), p.wait_ended())
            .await
            .unwrap();
        assert!(m.get(&p.id).await.is_some());
        {
            let mut state = p.state.lock().await;
            state.ended_at = Some(Instant::now() - Duration::from_secs(2));
        }
        m.maintenance().await;
        assert!(m.get(&p.id).await.is_none());
    }
}
