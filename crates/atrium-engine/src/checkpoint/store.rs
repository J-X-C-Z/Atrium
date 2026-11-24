// Ported from apps/server/src/checkpointing/CheckpointStore.ts and the Git
// checkpoint operations in apps/server/src/vcs/GitVcsDriver.ts
// (T3 Code v0.1.11, MIT).
//!
//! Checkpoints over hidden Git refs. Capturing, restoring and the worktree
//! snapshot reuse Atrium's `atrium_project::repo` helpers; the ref-to-ref diff
//! and reflog-based authorship scan are ported here.

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use atrium_project::repo;

use crate::contracts::CheckpointRef;

use super::errors::CheckpointStoreError;

/// T3's `CHECKPOINT_DIFF_MAX_OUTPUT_BYTES`.
pub const CHECKPOINT_DIFF_MAX_OUTPUT_BYTES: usize = 10_000_000;
/// Newest-first HEAD moves read when detecting a branch switch.
const HEAD_MOVES_SCAN_LIMIT: usize = 512;
const HEAD_MARK_TRAILER: &str = "Atrium-Head-Moved-At";
/// T3 keeps the patch prefixes explicit so user config cannot change what
/// clients parse.
const PATCH_RENDER_PREFIX_ARGS: [&str; 2] = ["--src-prefix=a/", "--dst-prefix=b/"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffFormat {
    Patch,
    Numstat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureCheckpointInput {
    pub cwd: PathBuf,
    pub checkpoint_ref: CheckpointRef,
    /// The checkpoint this one follows, so a turn that switched branches starts
    /// where it landed.
    pub previous_checkpoint_ref: Option<CheckpointRef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreCheckpointInput {
    pub cwd: PathBuf,
    pub checkpoint_ref: CheckpointRef,
    pub fallback_to_head: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffCheckpointsInput {
    pub cwd: PathBuf,
    pub from_checkpoint_ref: CheckpointRef,
    pub to_checkpoint_ref: CheckpointRef,
    pub fallback_from_to_head: bool,
    pub ignore_whitespace: bool,
    pub format: DiffFormat,
    /// Maximum bytes collected from the diff command.
    pub max_output_bytes: Option<usize>,
    /// Limits the diff to these exact paths. An empty list yields an empty diff.
    pub file_paths: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListAuthoredPathsInput {
    pub cwd: PathBuf,
    pub from_checkpoint_ref: CheckpointRef,
    pub to_checkpoint_ref: CheckpointRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteCheckpointRefsInput {
    pub cwd: PathBuf,
    pub checkpoint_refs: Vec<CheckpointRef>,
}

/// The checkpoint store's operations, mirroring T3's `VcsCheckpointOps`.
pub trait CheckpointStore {
    fn is_git_repository(&self, cwd: &Path) -> Result<bool, CheckpointStoreError>;
    fn capture_checkpoint(
        &self,
        input: &CaptureCheckpointInput,
    ) -> Result<(), CheckpointStoreError>;
    fn has_checkpoint_ref(
        &self,
        cwd: &Path,
        checkpoint_ref: &CheckpointRef,
    ) -> Result<bool, CheckpointStoreError>;
    fn restore_checkpoint(
        &self,
        input: &RestoreCheckpointInput,
    ) -> Result<bool, CheckpointStoreError>;
    fn diff_checkpoints(
        &self,
        input: &DiffCheckpointsInput,
    ) -> Result<String, CheckpointStoreError>;
    /// `None` means HEAD is unchanged or unrecorded; all changed paths then
    /// belong to the turn.
    fn list_authored_paths(
        &self,
        input: &ListAuthoredPathsInput,
    ) -> Result<Option<HashSet<String>>, CheckpointStoreError>;
    fn delete_checkpoint_refs(
        &self,
        input: &DeleteCheckpointRefsInput,
    ) -> Result<(), CheckpointStoreError>;
}

/// The Git-backed store.
#[derive(Clone, Copy, Debug, Default)]
pub struct GitCheckpointStore;

impl CheckpointStore for GitCheckpointStore {
    fn is_git_repository(&self, cwd: &Path) -> Result<bool, CheckpointStoreError> {
        let output = command(cwd, &["rev-parse", "--is-inside-work-tree"])
            .output()
            .map_err(|error| CheckpointStoreError::Other(error.to_string()))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim() == "true");
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not a git repository") || stderr.contains("not a work tree") {
            return Ok(false);
        }
        Err(process_exit(
            "CheckpointStore.isGitRepository",
            "git rev-parse",
            cwd,
            &output,
            "not a git repository",
        ))
    }

    fn capture_checkpoint(
        &self,
        input: &CaptureCheckpointInput,
    ) -> Result<(), CheckpointStoreError> {
        let root = repo_root(input.cwd.as_path())?;
        // A workspace may be a subdirectory; stage only what it owns.
        let tree = repo::snapshot_tree_at(&root, input.cwd.as_path()).map_err(other)?;
        let ref_name = input.checkpoint_ref.as_str();

        let head = resolve_commit(&root, "HEAD");
        let head_moves = if head.is_some() {
            read_head_moves(&root)
        } else {
            Vec::new()
        };
        let head_mark = head_moves.first().and_then(|last| {
            (head.as_deref() == Some(last.to.as_str())).then(|| {
                let count = head_moves.iter().filter(|mv| mv.at == last.at).count();
                format!("{} {}", last.at, count)
            })
        });

        let turn_start = input
            .previous_checkpoint_ref
            .as_ref()
            .filter(|previous| repo::has_reference(&root, previous.as_str()))
            .and_then(|previous| branch_switch_turn_start(&root, previous, &head_moves));

        let mut parents: Vec<&str> = Vec::new();
        if let Some(head) = head.as_deref() {
            parents.push(head);
        }
        if let Some(start) = turn_start.as_deref() {
            parents.push(start);
        }
        let message = match head_mark.as_deref() {
            Some(mark) => {
                format!("atrium checkpoint ref={ref_name}\n\n{HEAD_MARK_TRAILER}: {mark}")
            }
            None => format!("atrium checkpoint ref={ref_name}"),
        };
        repo::commit_tree_reference(&root, ref_name, &tree, &message, &parents, None)
            .map_err(other)?;
        Ok(())
    }

    fn has_checkpoint_ref(
        &self,
        cwd: &Path,
        checkpoint_ref: &CheckpointRef,
    ) -> Result<bool, CheckpointStoreError> {
        let root = repo_root(cwd)?;
        Ok(repo::has_reference(&root, checkpoint_ref.as_str()))
    }

    fn restore_checkpoint(
        &self,
        input: &RestoreCheckpointInput,
    ) -> Result<bool, CheckpointStoreError> {
        let root = repo_root(input.cwd.as_path())?;
        let mut commit = repo::resolve_reference(&root, input.checkpoint_ref.as_str());
        if commit.is_none() && input.fallback_to_head {
            commit = resolve_commit(&root, "HEAD");
        }
        let Some(commit) = commit else {
            return Ok(false);
        };
        let tree = repo::tree_of_commit(&root, &commit)
            .ok_or_else(|| other(format!("could not resolve tree of {commit}")))?;
        repo::restore_worktree_to_tree(&root, &tree).map_err(other)?;
        Ok(true)
    }

    fn diff_checkpoints(
        &self,
        input: &DiffCheckpointsInput,
    ) -> Result<String, CheckpointStoreError> {
        if input.file_paths.as_ref().is_some_and(Vec::is_empty) {
            return Ok(String::new());
        }
        let root = repo_root(input.cwd.as_path())?;
        let mut from =
            resolve_turn_start(&root, &input.from_checkpoint_ref, &input.to_checkpoint_ref);

        if input.fallback_from_to_head
            && !repo::has_reference(&root, input.from_checkpoint_ref.as_str())
        {
            from = resolve_commit(&root, "HEAD").ok_or_else(|| {
                process_exit_detail(
                    "CheckpointStore.diffCheckpoints",
                    "git diff",
                    &root,
                    1,
                    "Checkpoint ref is unavailable for diff operation.",
                )
            })?;
        }

        let mut args: Vec<String> = vec![
            "-c".into(),
            "diff.relative=false".into(),
            "-c".into(),
            "diff.suppressBlankEmpty=false".into(),
            "diff".into(),
        ];
        match input.format {
            DiffFormat::Numstat => {
                args.push("--numstat".into());
                args.push("-z".into());
            }
            DiffFormat::Patch => args.push("--patch".into()),
        }
        args.extend(
            [
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--submodule=short",
                PATCH_RENDER_PREFIX_ARGS[0],
                PATCH_RENDER_PREFIX_ARGS[1],
            ]
            .into_iter()
            .map(str::to_string),
        );
        if input.ignore_whitespace {
            args.push("--ignore-all-space".into());
        }
        args.push(format!("{from}^{{commit}}"));
        args.push(format!("{}^{{commit}}", input.to_checkpoint_ref));
        if let Some(paths) = input.file_paths.as_ref() {
            args.push("--".into());
            args.extend(paths.iter().map(|path| format!(":(top,literal){path}")));
        }

        let output = run_git_strings(&root, &args)?;
        if !output.status.success() {
            return Err(process_exit(
                "CheckpointStore.diffCheckpoints",
                "git diff",
                &root,
                &output,
                "Checkpoint ref is unavailable for diff operation.",
            ));
        }
        let max = input
            .max_output_bytes
            .unwrap_or(CHECKPOINT_DIFF_MAX_OUTPUT_BYTES);
        let stdout = output.stdout;
        if input.format == DiffFormat::Numstat && stdout.len() > max {
            return Err(process_exit_detail(
                "CheckpointStore.diffCheckpoints",
                "git diff",
                &root,
                1,
                "git diff output exceeded the maximum size.",
            ));
        }
        let bytes = if stdout.len() > max {
            &stdout[..max]
        } else {
            &stdout[..]
        };
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }

    fn list_authored_paths(
        &self,
        input: &ListAuthoredPathsInput,
    ) -> Result<Option<HashSet<String>>, CheckpointStoreError> {
        let root = repo_root(input.cwd.as_path())?;
        let turn_start =
            resolve_turn_start(&root, &input.from_checkpoint_ref, &input.to_checkpoint_ref);

        let heads = run_git(
            &root,
            &[
                "-c",
                "log.showSignature=false",
                "log",
                "--no-walk=unsorted",
                "--format=%ct %P",
                &format!("{turn_start}^{{commit}}"),
                &format!("{}^{{commit}}", input.to_checkpoint_ref),
            ],
        )?;
        if !heads.status.success() {
            return Err(process_exit(
                "CheckpointStore.listAuthoredPaths",
                "git log",
                &root,
                &heads,
                "could not read checkpoint heads",
            ));
        }
        let heads = String::from_utf8_lossy(&heads.stdout);
        let mut lines = heads.trim_end().split('\n');
        let from_line = lines.next().unwrap_or("");
        let to_line = lines.next().unwrap_or("");
        let mut from_parts = from_line.split(' ');
        let captured_at = from_parts.next().unwrap_or("");
        let start_head = from_parts.next().unwrap_or("");
        let end_head = to_line.split(' ').nth(1).unwrap_or("");
        if start_head.is_empty() || end_head.is_empty() || start_head == end_head {
            return Ok(None);
        }

        let list_lines =
            |args: &[&str], stdin: Option<&str>| -> Result<Vec<String>, CheckpointStoreError> {
                let mut git_args = vec!["-c", "log.showSignature=false"];
                git_args.extend_from_slice(args);
                let output = match stdin {
                    Some(stdin) => run_git_stdin(&root, &git_args, Some(stdin))?,
                    None => run_git(&root, &git_args)?,
                };
                if !output.status.success() {
                    return Err(process_exit(
                        "CheckpointStore.listAuthoredPaths",
                        "git log",
                        &root,
                        &output,
                        "git log failed",
                    ));
                }
                Ok(String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect())
            };
        let list_paths = |args: &[&str],
                          stdin: Option<&str>|
         -> Result<Vec<String>, CheckpointStoreError> {
            let mut git_args = vec!["-c", "diff.relative=false", "-c", "log.showSignature=false"];
            git_args.extend_from_slice(args);
            git_args.extend_from_slice(&["--name-only", "-z", "--no-renames", "--no-ext-diff"]);
            let output = match stdin {
                Some(stdin) => run_git_stdin(&root, &git_args, Some(stdin))?,
                None => run_git(&root, &git_args)?,
            };
            if !output.status.success() {
                return Err(process_exit(
                    "CheckpointStore.listAuthoredPaths",
                    "git diff/log",
                    &root,
                    &output,
                    "git path listing failed",
                ));
            }
            Ok(String::from_utf8_lossy(&output.stdout)
                .split('\0')
                .map(str::to_string)
                .collect())
        };

        let list_imported_commits = || -> Result<HashSet<String>, CheckpointStoreError> {
            let entries: Vec<ReflogEntry> = list_lines(
                &[
                    "log",
                    "--walk-reflogs",
                    "--all",
                    &format!("--since=@{}", since_second(captured_at)),
                    "--format=%H %gD %gs",
                ],
                None,
            )?
            .iter()
            .map(|line| parse_reflog_entry(line))
            .collect();
            let fetched_tips: HashSet<String> = entries
                .iter()
                .filter(|entry| entry.fetched && entry.subject != "update by push")
                .map(|entry| entry.commit.clone())
                .collect();
            if fetched_tips.is_empty() {
                return Ok(fetched_tips);
            }
            let created_here: HashSet<String> = entries
                .iter()
                .filter(|entry| is_local_reflog_creation(entry))
                .map(|entry| entry.commit.clone())
                .collect();
            let mut stdin = fetched_tips.into_iter().collect::<Vec<_>>();
            stdin.push(format!("^{start_head}"));
            let fetched = list_lines(
                &[
                    "rev-list",
                    &format!("--since=@{}", since_second(captured_at)),
                    "--stdin",
                ],
                Some(&format!("{}\n", stdin.join("\n"))),
            )?;
            Ok(fetched
                .into_iter()
                .filter(|commit| !created_here.contains(commit))
                .collect())
        };

        let recent_commits = list_lines(
            &[
                "rev-list",
                &format!("--since=@{}", since_second(captured_at)),
                &format!("{start_head}..{end_head}"),
            ],
            None,
        )?;
        let mut path_lists = Vec::new();
        path_lists.extend(list_paths(
            &["diff", start_head, &format!("{turn_start}^{{commit}}")],
            None,
        )?);
        path_lists.extend(list_paths(
            &[
                "diff",
                end_head,
                &format!("{}^{{commit}}", input.to_checkpoint_ref),
            ],
            None,
        )?);
        path_lists.extend(list_paths(
            &[
                "log",
                "--format=",
                "--diff-merges=remerge",
                &format!("{end_head}..{start_head}"),
            ],
            None,
        )?);

        let mut unproven: HashSet<String> = recent_commits.iter().cloned().collect();
        if !recent_commits.is_empty()
            && recent_commits.len() <= HEAD_MOVES_SCAN_LIMIT
            && let Ok(lines) = list_lines(
                &[
                    "log",
                    "--walk-reflogs",
                    &format!("--max-count={HEAD_MOVES_SCAN_LIMIT}"),
                    "HEAD",
                    &format!("--since=@{}", since_second(captured_at)),
                    "--format=%H %gD %gs",
                ],
                None,
            )
        {
            for line in lines {
                let entry = parse_reflog_entry(&line);
                if is_local_reflog_creation(&entry) {
                    unproven.remove(&entry.commit);
                }
            }
        }

        let imported_commits = if unproven.is_empty() {
            HashSet::new()
        } else {
            list_imported_commits().unwrap_or_default()
        };
        let own_commits: Vec<String> = recent_commits
            .into_iter()
            .filter(|commit| !imported_commits.contains(commit))
            .collect();
        let mut paths: HashSet<String> = path_lists
            .into_iter()
            .filter(|path| !path.is_empty())
            .collect();
        if !own_commits.is_empty() {
            let stdin = format!("{}\n", own_commits.join("\n"));
            for path in list_paths(
                &[
                    "log",
                    "--no-walk=unsorted",
                    "--stdin",
                    "--format=",
                    "--diff-merges=remerge",
                ],
                Some(&stdin),
            )? {
                if !path.is_empty() {
                    paths.insert(path);
                }
            }
        }
        Ok(Some(paths))
    }

    fn delete_checkpoint_refs(
        &self,
        input: &DeleteCheckpointRefsInput,
    ) -> Result<(), CheckpointStoreError> {
        let root = repo_root(input.cwd.as_path())?;
        for checkpoint_ref in &input.checkpoint_refs {
            // T3 treats a delete failure as best-effort cleanup.
            let _ = repo::delete_reference(&root, checkpoint_ref.as_str());
        }
        Ok(())
    }
}

// ---- git plumbing --------------------------------------------------------

fn command(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .args(args)
        .stdin(Stdio::null())
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_PREFIX"] {
        cmd.env_remove(key);
    }
    cmd
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<Output, CheckpointStoreError> {
    command(cwd, args)
        .output()
        .map_err(|error| CheckpointStoreError::Other(error.to_string()))
}

fn run_git_strings(cwd: &Path, args: &[String]) -> Result<Output, CheckpointStoreError> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run_git(cwd, &args)
}

fn run_git_stdin(
    cwd: &Path,
    args: &[&str],
    stdin: Option<&str>,
) -> Result<Output, CheckpointStoreError> {
    let mut cmd = command(cwd, args);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|error| CheckpointStoreError::Other(error.to_string()))?;
    if let Some(input) = stdin {
        child
            .stdin
            .as_mut()
            .expect("stdin was piped")
            .write_all(input.as_bytes())
            .map_err(|error| CheckpointStoreError::Other(error.to_string()))?;
    }
    child
        .wait_with_output()
        .map_err(|error| CheckpointStoreError::Other(error.to_string()))
}

fn run_git_stdin_with_index(
    cwd: &Path,
    index: &Path,
    args: &[&str],
    stdin: &str,
) -> Result<Output, CheckpointStoreError> {
    let mut cmd = command(cwd, args);
    cmd.env("GIT_INDEX_FILE", index)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|error| CheckpointStoreError::Other(error.to_string()))?;
    child
        .stdin
        .as_mut()
        .expect("stdin was piped")
        .write_all(stdin.as_bytes())
        .map_err(|error| CheckpointStoreError::Other(error.to_string()))?;
    child
        .wait_with_output()
        .map_err(|error| CheckpointStoreError::Other(error.to_string()))
}

fn repo_root(cwd: &Path) -> Result<PathBuf, CheckpointStoreError> {
    repo::root(cwd).ok_or_else(|| {
        process_exit_detail(
            "CheckpointStore",
            "git rev-parse",
            cwd,
            128,
            "not a git repository",
        )
    })
}

fn resolve_commit(root: &Path, rev: &str) -> Option<String> {
    repo::resolve_reference(root, rev)
}

fn process_exit(
    operation: &str,
    command: &str,
    cwd: &Path,
    output: &Output,
    fallback: &str,
) -> CheckpointStoreError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| fallback.to_string());
    CheckpointStoreError::process_exit(
        operation,
        command,
        cwd.display().to_string(),
        output.status.code().unwrap_or(1),
        detail,
    )
}

fn process_exit_detail(
    operation: &str,
    command: &str,
    cwd: &Path,
    exit_code: i32,
    detail: &str,
) -> CheckpointStoreError {
    CheckpointStoreError::process_exit(
        operation,
        command,
        cwd.display().to_string(),
        exit_code,
        detail,
    )
}

fn other(error: impl std::fmt::Display) -> CheckpointStoreError {
    CheckpointStoreError::Other(format!("{error:#}"))
}

/// T3 compares against `--since=@<committer-seconds>`; Git treats that as
/// strictly newer, so a commit made in the checkpoint's own second would go
/// unseen. Back up one second so the comparison is inclusive.
fn since_second(captured_at: &str) -> u64 {
    captured_at.parse::<u64>().unwrap_or(0).saturating_sub(1)
}

// ---- checkpoint heads ----------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct HeadMove {
    to: String,
    at: String,
    reason: String,
}

/// Newest first: the commit each HEAD move landed on, when, and Git's reason.
fn read_head_moves(root: &Path) -> Vec<HeadMove> {
    let output = match run_git(
        root,
        &[
            "-c",
            "log.showSignature=false",
            "log",
            "--walk-reflogs",
            "-n",
            &HEAD_MOVES_SCAN_LIMIT.to_string(),
            "--date=unix",
            "--format=%H %gd %gs",
            "HEAD",
            "--",
        ],
    ) {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().filter_map(parse_head_move).collect()
}

fn parse_head_move(line: &str) -> Option<HeadMove> {
    let (to, rest) = line.split_once(' ')?;
    let (selector, reason) = rest.split_once(' ')?;
    if to.is_empty() || !to.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let at = selector
        .strip_prefix("HEAD@{")?
        .strip_suffix('}')?
        .to_string();
    Some(HeadMove {
        to: to.to_string(),
        at,
        reason: reason.to_string(),
    })
}

/// Rebuilds the files a branch switch replaced, so a turn's diff starts on the
/// branch it landed on rather than on the previous checkpoint's branch.
fn branch_switch_turn_start(
    root: &Path,
    previous_ref: &CheckpointRef,
    head_moves: &[HeadMove],
) -> Option<String> {
    let previous = run_git(
        root,
        &[
            "-c",
            "log.showSignature=false",
            "log",
            "--no-walk",
            "--format=%H %T %ct %P%n%B",
            &format!("{}^{{commit}}", previous_ref),
        ],
    )
    .ok()?;
    if !previous.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&previous.stdout);
    let (first, body) = match text.split_once('\n') {
        Some((first, body)) => (first, body),
        None => (text.as_ref(), ""),
    };
    let mut parts = first.split(' ');
    let _commit = parts.next()?;
    let tree = parts.next()?;
    let _captured_at = parts.next()?;
    let previous_head = parts.next().unwrap_or("");
    let (marked_at, marked_count) = parse_head_mark(body)?;

    let oldest_marked = head_moves.iter().rposition(|mv| mv.at == marked_at)?;
    if oldest_marked == HEAD_MOVES_SCAN_LIMIT - 1 {
        return None;
    }
    let since = (oldest_marked as isize) - (marked_count as isize) + 1;
    if since < 0 {
        return None;
    }
    let since = since as usize;
    if head_moves.get(since)?.to != previous_head {
        return None;
    }

    let mut switches: Vec<(String, String)> = Vec::new();
    for index in 0..since {
        let mv = &head_moves[index];
        let from = &head_moves[index + 1].to;
        if mv.reason.starts_with("checkout: moving from ") && from != &mv.to {
            switches.push((from.clone(), mv.to.clone()));
        }
    }
    switches.reverse();
    let landed_on = switches.last()?.1.clone();

    let temp = tempfile::tempdir().ok()?;
    let index = temp.path().join("index");
    let git_index = |args: &[&str]| -> Option<Output> {
        command(root, args)
            .env("GIT_INDEX_FILE", &index)
            .output()
            .ok()
    };
    let read = git_index(&["read-tree", tree])?;
    if !read.status.success() {
        return None;
    }
    for (from, to) in &switches {
        let changes = run_git(root, &["diff-tree", "-r", "--no-renames", from, to]).ok()?;
        if !changes.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&changes.stdout);
        let mut entries = String::new();
        for line in text.lines() {
            let Some((meta, path)) = line.split_once('\t') else {
                continue;
            };
            let modes: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
            if modes.len() < 3 {
                continue;
            }
            let old_mode = modes[0];
            let new_mode = modes[1];
            let Some(new_hash) = modes.get(3) else {
                continue;
            };
            if old_mode == "160000" || new_mode == "160000" {
                continue;
            }
            entries.push_str(&format!("{new_mode} {new_hash}\t{path}\n"));
        }
        if entries.is_empty() {
            continue;
        }
        let update =
            run_git_stdin_with_index(root, &index, &["update-index", "--index-info"], &entries)
                .ok()?;
        if !update.status.success() {
            return None;
        }
    }
    let start_tree = git_index(&["write-tree"])?;
    if !start_tree.status.success() {
        return None;
    }
    let start_tree = String::from_utf8_lossy(&start_tree.stdout)
        .trim()
        .to_string();
    if start_tree.is_empty() || start_tree == tree {
        return None;
    }

    let message = format!("atrium checkpoint turn start ref={previous_ref}");
    let start = run_git(
        root,
        &[
            "commit-tree",
            &start_tree,
            "-p",
            &landed_on,
            "-p",
            previous_ref.as_str(),
            "-m",
            &message,
        ],
    )
    .ok()?;
    if !start.status.success() {
        return None;
    }
    let start = String::from_utf8_lossy(&start.stdout).trim().to_string();
    if start.is_empty() {
        return None;
    }
    Some(start)
}

fn parse_head_mark(body: &str) -> Option<(String, usize)> {
    let prefix = format!("{HEAD_MARK_TRAILER}: ");
    body.lines().find_map(|line| {
        let rest = line.strip_prefix(&prefix)?;
        let (at, count) = rest.split_once(' ')?;
        Some((at.to_string(), count.parse().ok()?))
    })
}

/// The commit a turn started from when a checkpoint has a synthetic second
/// parent for a branch switch; otherwise `from` itself.
fn resolve_turn_start(root: &Path, from: &CheckpointRef, to: &CheckpointRef) -> String {
    let output = run_git(
        root,
        &[
            "-c",
            "log.showSignature=false",
            "log",
            "--no-walk=unsorted",
            "--format=%H %P",
            &format!("{from}^{{commit}}"),
            &format!("{to}^2"),
        ],
    );
    let Ok(output) = output else {
        return from.as_str().to_string();
    };
    if !output.status.success() {
        return from.as_str().to_string();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.trim_end().split('\n');
    let from_line = lines.next().unwrap_or("");
    let start_line = lines.next().unwrap_or("");
    let mut from_parts = from_line.split(' ');
    let from_commit = from_parts.next().unwrap_or("");
    let mut start_parts = start_line.split(' ');
    let start = start_parts.next().unwrap_or("");
    let _first_parent = start_parts.next();
    // The synthetic start lists the previous checkpoint as its second parent.
    let started_from = start_parts.next().unwrap_or("");
    if !start.is_empty() && started_from == from_commit {
        start.to_string()
    } else {
        from.as_str().to_string()
    }
}

// ---- reflog authorship ---------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReflogEntry {
    commit: String,
    fetched: bool,
    subject: String,
}

fn parse_reflog_entry(line: &str) -> ReflogEntry {
    let mut parts = line.split(' ');
    let commit = parts.next().unwrap_or("").to_string();
    let selector = parts.next().unwrap_or("");
    let subject = parts.collect::<Vec<_>>().join(" ");
    ReflogEntry {
        commit,
        fetched: selector.starts_with("refs/remotes/"),
        subject,
    }
}

/// Reflog subjects that only point a ref at an existing commit. Anything else
/// counts as creating one, so an unknown subject keeps its commit attributed.
fn is_ref_move_subject(subject: &str) -> bool {
    if subject.is_empty() {
        return true;
    }
    let lower = subject.to_ascii_lowercase();
    for verb in ["checkout", "reset", "branch", "clone", "fetch"] {
        if let Some(rest) = lower.strip_prefix(verb)
            && rest
                .chars()
                .next()
                .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
        {
            return true;
        }
    }
    if has_bracketed_phase(&lower) {
        return true;
    }
    for verb in ["merge", "pull", "rebase", "cherry-pick"] {
        if let Some(rest) = lower.strip_prefix(verb)
            && rest
                .chars()
                .next()
                .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
            && rest.ends_with(": fast-forward")
        {
            return true;
        }
    }
    false
}

fn has_bracketed_phase(subject: &str) -> bool {
    let Some(open) = subject.find('(') else {
        return false;
    };
    if subject[..open].contains(':') {
        return false;
    }
    subject[open..].starts_with("(start): ")
        || subject[open..].starts_with("(finish): ")
        || subject[open..].starts_with("(abort): ")
        || subject[open..].starts_with("(reset): ")
}

fn is_local_reflog_creation(entry: &ReflogEntry) -> bool {
    !entry.fetched && !is_ref_move_subject(&entry.subject)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::ThreadId;
    use std::fs;
    use std::path::PathBuf;

    // Ported from apps/server/src/checkpointing/CheckpointStore.test.ts
    // (T3 Code v0.1.11, MIT).

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = command(cwd, args).output().expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn git_env(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let mut cmd = command(cwd, args);
        for (key, value) in env {
            cmd.env(key, value);
        }
        let output = cmd.output().expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        git(&path, &["init"]);
        git(&path, &["config", "user.email", "test@test.com"]);
        git(&path, &["config", "user.name", "Test"]);
        fs::write(path.join("README.md"), "# test\n").unwrap();
        git(&path, &["add", "."]);
        git(&path, &["commit", "-m", "initial commit"]);
        (dir, path)
    }

    fn checkpoint_ref(thread: &str, turn: usize) -> CheckpointRef {
        CheckpointRef::new(format!("refs/t3/checkpoints/{thread}/turn/{turn}"))
    }

    fn capture(cwd: &Path, reference: &CheckpointRef) {
        GitCheckpointStore
            .capture_checkpoint(&CaptureCheckpointInput {
                cwd: cwd.to_path_buf(),
                checkpoint_ref: reference.clone(),
                previous_checkpoint_ref: None,
            })
            .unwrap();
    }

    fn capture_after(cwd: &Path, reference: &CheckpointRef, previous: &CheckpointRef) {
        GitCheckpointStore
            .capture_checkpoint(&CaptureCheckpointInput {
                cwd: cwd.to_path_buf(),
                checkpoint_ref: reference.clone(),
                previous_checkpoint_ref: Some(previous.clone()),
            })
            .unwrap();
    }

    fn diff(
        cwd: &Path,
        from: &CheckpointRef,
        to: &CheckpointRef,
        ignore_whitespace: bool,
    ) -> String {
        GitCheckpointStore
            .diff_checkpoints(&DiffCheckpointsInput {
                cwd: cwd.to_path_buf(),
                from_checkpoint_ref: from.clone(),
                to_checkpoint_ref: to.clone(),
                fallback_from_to_head: false,
                ignore_whitespace,
                format: DiffFormat::Patch,
                max_output_bytes: None,
                file_paths: None,
            })
            .unwrap()
    }

    fn numstat(cwd: &Path, from: &CheckpointRef, to: &CheckpointRef, ignore: bool) -> String {
        GitCheckpointStore
            .diff_checkpoints(&DiffCheckpointsInput {
                cwd: cwd.to_path_buf(),
                from_checkpoint_ref: from.clone(),
                to_checkpoint_ref: to.clone(),
                fallback_from_to_head: false,
                ignore_whitespace: ignore,
                format: DiffFormat::Numstat,
                max_output_bytes: None,
                file_paths: None,
            })
            .unwrap()
    }

    fn authored(cwd: &Path, from: &CheckpointRef, to: &CheckpointRef) -> Option<HashSet<String>> {
        GitCheckpointStore
            .list_authored_paths(&ListAuthoredPathsInput {
                cwd: cwd.to_path_buf(),
                from_checkpoint_ref: from.clone(),
                to_checkpoint_ref: to.clone(),
            })
            .unwrap()
    }

    #[test]
    fn detects_a_nested_workspace_without_its_own_git_entry() {
        let (_guard, tmp) = repo();
        let nested = tmp.join("packages").join("nested");
        fs::create_dir_all(&nested).unwrap();
        assert!(GitCheckpointStore.is_git_repository(&nested).unwrap());
    }

    #[test]
    fn fails_the_repository_probe_that_other_callers_read_as_no_repository() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path();
        git(tmp, &["init"]);
        fs::write(tmp.join(".git").join("config"), "invalid config\n").unwrap();
        assert!(GitCheckpointStore.is_git_repository(tmp).is_err());
    }

    #[test]
    fn returns_full_oversized_checkpoint_diffs_without_truncation() {
        let (_guard, tmp) = repo();
        let thread = ThreadId::new("thread-checkpoint-store");
        let from = checkpoint_ref(thread.as_str(), 0);
        let to = checkpoint_ref(thread.as_str(), 1);
        capture(&tmp, &from);
        fs::write(
            tmp.join("README.md"),
            (0..5_000)
                .map(|index| format!("line {index:05}"))
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        capture(&tmp, &to);
        let patch = diff(&tmp, &from, &to, true);
        assert!(patch.contains("diff --git"));
        assert!(!patch.contains("[truncated]"));
        assert!(patch.contains("+line 04999"));
    }

    #[test]
    fn keeps_the_patch_format_clients_parse_when_the_repository_changes_it() {
        let (_guard, tmp) = repo();
        git(&tmp, &["config", "diff.noprefix", "true"]);
        git(&tmp, &["config", "diff.suppressBlankEmpty", "true"]);
        git(&tmp, &["config", "diff.submodule", "log"]);
        let sub = tmp.join("sub");
        fs::create_dir_all(&sub).unwrap();
        git(&sub, &["init"]);
        fs::write(sub.join("README.md"), "# sub\n").unwrap();
        git(&sub, &["add", "."]);
        git(&sub, &["commit", "-m", "sub"]);
        let thread = ThreadId::new("thread-checkpoint-store-noprefix");
        let from = checkpoint_ref(thread.as_str(), 0);
        let to = checkpoint_ref(thread.as_str(), 1);
        fs::write(tmp.join("README.md"), "# test\n\nbefore\n").unwrap();
        capture(&tmp, &from);
        fs::write(tmp.join("README.md"), "# test\n\nafter\n").unwrap();
        git(&sub, &["commit", "--allow-empty", "-m", "moved"]);
        capture(&tmp, &to);
        let patch = diff(&tmp, &from, &to, false);
        assert!(patch.contains("diff --git a/README.md b/README.md"));
        assert!(patch.contains(" # test\n \n-before\n+after\n"));
        assert!(patch.contains("diff --git a/sub b/sub"));
        assert!(patch.contains("+Subproject commit "));
    }

    #[test]
    fn can_hide_indentation_churn_when_changes_wrap_existing_lines() {
        let (_guard, tmp) = repo();
        let thread = ThreadId::new("thread-checkpoint-store-whitespace");
        let from = checkpoint_ref(thread.as_str(), 0);
        let to = checkpoint_ref(thread.as_str(), 1);
        let component = tmp.join("Component.tsx");
        fs::write(
            &component,
            [
                "export function View() {",
                "  return (",
                "    <section>",
                "      <h1>Title</h1>",
                "      <p>Body</p>",
                "    </section>",
                "  );",
                "}",
                "",
            ]
            .join("\n"),
        )
        .unwrap();
        capture(&tmp, &from);
        fs::write(
            &component,
            [
                "export function View() {",
                "  return (",
                "    <section>",
                "      {isReady ? (",
                "        <div>",
                "          <h1>Title</h1>",
                "          <p>Body</p>",
                "        </div>",
                "      ) : null}",
                "    </section>",
                "  );",
                "}",
                "",
            ]
            .join("\n"),
        )
        .unwrap();
        capture(&tmp, &to);

        let normal = diff(&tmp, &from, &to, false);
        assert!(normal.contains("diff --git"));
        assert!(normal.contains("-      <h1>Title</h1>"));
        assert!(normal.contains("+          <h1>Title</h1>"));
        let ignored = diff(&tmp, &from, &to, true);
        assert!(ignored.contains("diff --git"));
        assert!(ignored.contains("+      {isReady ? ("));
        assert!(ignored.contains("+        <div>"));
        assert!(!ignored.contains("-      <h1>Title</h1>"));
        assert!(!ignored.contains("+          <h1>Title</h1>"));
        for ignore in [false, true] {
            assert_eq!(
                parse_turn_diff_files_from_numstat(&numstat(&tmp, &from, &to, ignore)),
                vec![super::super::diffs::TurnDiffFileSummary {
                    path: "Component.tsx".into(),
                    previous_path: None,
                    additions: if ignore { 4 } else { 6 },
                    deletions: if ignore { 0 } else { 2 },
                }]
            );
        }
    }

    #[test]
    fn counts_changes_whose_full_patch_exceeds_the_output_limit() {
        let (_guard, tmp) = repo();
        let thread = ThreadId::new("large-checkpoint-summary");
        let from = checkpoint_ref(thread.as_str(), 0);
        let to = checkpoint_ref(thread.as_str(), 1);
        let file = tmp.join("README.md");
        let line_count = 20_000;
        fs::write(
            &file,
            format!("{}\n", "before".repeat(50)).repeat(line_count),
        )
        .unwrap();
        capture(&tmp, &from);
        fs::write(
            &file,
            format!("{}\n", "after".repeat(60)).repeat(line_count),
        )
        .unwrap();
        capture(&tmp, &to);
        let output = numstat(&tmp, &from, &to, false);
        assert_eq!(
            parse_turn_diff_files_from_numstat(&output),
            vec![super::super::diffs::TurnDiffFileSummary {
                path: "README.md".into(),
                previous_path: None,
                additions: line_count as u32,
                deletions: line_count as u32,
            }]
        );
        assert!(output.len() < 100);
    }

    #[test]
    fn preserves_file_paths_and_turn_ranges_without_changing_the_user_index() {
        let (_guard, tmp) = repo();
        git(&tmp, &["config", "diff.renames", "copies"]);
        let thread = ThreadId::new("checkpoint-summary-paths");
        let baseline = checkpoint_ref(thread.as_str(), 0);
        let first_turn = checkpoint_ref(thread.as_str(), 1);
        let second_turn = checkpoint_ref(thread.as_str(), 2);
        let copied_text: String = (0..20)
            .map(|index| format!("copy line {index}\n"))
            .collect();
        let renamed_path = "renamed\tcafé\nname.txt";
        let added_path = "new\tfile\n名.txt";
        for (path, contents) in [
            ("copy-source.txt", copied_text.as_str()),
            ("deleted.txt", "delete me\n"),
            ("rename-old.txt", "before\nkeep one\nkeep two\nkeep three\n"),
            ("binary.bin", "\0before"),
        ] {
            fs::write(tmp.join(path), contents).unwrap();
        }
        capture(&tmp, &baseline);
        fs::rename(tmp.join("rename-old.txt"), tmp.join(renamed_path)).unwrap();
        fs::remove_file(tmp.join("deleted.txt")).unwrap();
        for (path, contents) in [
            ("copy-source.txt", format!("{copied_text}one more\n")),
            ("copied.txt", copied_text.clone()),
            (
                renamed_path,
                "after\nkeep one\nkeep two\nkeep three\n".into(),
            ),
            ("binary.bin", "\0after".into()),
            ("empty.txt", String::new()),
            (added_path, "first\nsecond\n".into()),
        ] {
            fs::write(tmp.join(path), contents).unwrap();
        }
        capture(&tmp, &first_turn);
        let user_index = fs::read(tmp.join(".git").join("index")).unwrap();
        let first_summary =
            parse_turn_diff_files_from_numstat(&numstat(&tmp, &baseline, &first_turn, false));
        let mut expected = vec![
            summary("binary.bin", None, 0, 0),
            summary("copied.txt", Some("copy-source.txt"), 0, 0),
            summary("copy-source.txt", None, 1, 0),
            summary("deleted.txt", None, 0, 1),
            summary("empty.txt", None, 0, 0),
            summary(added_path, None, 2, 0),
            summary(renamed_path, Some("rename-old.txt"), 1, 1),
        ];
        expected.sort_by(|left, right| left.path.cmp(&right.path));
        assert_eq!(first_summary, expected);

        fs::remove_file(tmp.join("empty.txt")).unwrap();
        fs::write(tmp.join("copy-source.txt"), "replacement\n").unwrap();
        capture(&tmp, &second_turn);
        let second_summary =
            parse_turn_diff_files_from_numstat(&numstat(&tmp, &first_turn, &second_turn, false));
        assert_eq!(
            second_summary,
            vec![
                summary("copy-source.txt", None, 1, 21),
                summary("empty.txt", None, 0, 0),
            ]
        );

        let inclusive =
            parse_turn_diff_files_from_numstat(&numstat(&tmp, &baseline, &second_turn, false));
        let inclusive_expected: Vec<_> = expected
            .iter()
            .filter(|file| file.path != "empty.txt")
            .map(|file| {
                if file.path == "copy-source.txt" {
                    summary("copy-source.txt", None, 1, 20)
                } else {
                    file.clone()
                }
            })
            .collect();
        assert_eq!(inclusive, inclusive_expected);
        assert_eq!(numstat(&tmp, &baseline, &baseline, false), "");
        assert_eq!(
            fs::read(tmp.join(".git").join("index")).unwrap(),
            user_index
        );
    }

    fn summary(
        path: &str,
        previous_path: Option<&str>,
        additions: u32,
        deletions: u32,
    ) -> super::super::diffs::TurnDiffFileSummary {
        super::super::diffs::TurnDiffFileSummary {
            path: path.into(),
            previous_path: previous_path.map(str::to_string),
            additions,
            deletions,
        }
    }

    #[test]
    fn uses_head_for_a_missing_baseline_only_when_requested() {
        let (_guard, tmp) = repo();
        let thread = ThreadId::new("checkpoint-summary-fallback");
        let from = checkpoint_ref(thread.as_str(), 0);
        let to = checkpoint_ref(thread.as_str(), 1);
        fs::write(tmp.join("README.md"), "changed\n").unwrap();
        capture(&tmp, &to);
        let input = DiffCheckpointsInput {
            cwd: tmp.clone(),
            from_checkpoint_ref: from,
            to_checkpoint_ref: to,
            fallback_from_to_head: false,
            ignore_whitespace: false,
            format: DiffFormat::Numstat,
            max_output_bytes: None,
            file_paths: None,
        };
        assert!(GitCheckpointStore.diff_checkpoints(&input).is_err());
        let mut fallback = input.clone();
        fallback.fallback_from_to_head = true;
        assert_eq!(
            parse_turn_diff_files_from_numstat(
                &GitCheckpointStore.diff_checkpoints(&fallback).unwrap()
            ),
            vec![summary("README.md", None, 1, 1)]
        );
    }

    #[test]
    fn reads_repository_paths_from_a_subdirectory_workspace_under_diff_relative() {
        let (_guard, tmp) = repo();
        git(&tmp, &["config", "diff.relative", "true"]);
        let cwd = tmp.join("app");
        fs::create_dir_all(&cwd).unwrap();
        let thread = ThreadId::new("checkpoint-file-paths-subdirectory");
        let from = checkpoint_ref(thread.as_str(), 0);
        let to = checkpoint_ref(thread.as_str(), 1);
        capture(&cwd, &from);
        fs::write(cwd.join("kept.txt"), "kept\n").unwrap();
        git(&cwd, &["add", "kept.txt"]);
        git(&cwd, &["commit", "-m", "kept"]);
        fs::write(tmp.join("README.md"), "# hidden\n").unwrap();
        capture(&cwd, &to);
        let patch = GitCheckpointStore
            .diff_checkpoints(&DiffCheckpointsInput {
                cwd: cwd.clone(),
                from_checkpoint_ref: from.clone(),
                to_checkpoint_ref: to.clone(),
                fallback_from_to_head: false,
                ignore_whitespace: false,
                format: DiffFormat::Patch,
                max_output_bytes: None,
                file_paths: Some(vec!["app/kept.txt".into()]),
            })
            .unwrap();
        assert!(patch.contains("+++ b/app/kept.txt"));
        assert!(patch.contains("+kept"));
        assert!(!patch.contains("hidden"));
        assert_eq!(
            authored(&cwd, &from, &to),
            Some(["app/kept.txt".to_string()].into_iter().collect())
        );
    }

    #[test]
    fn leaves_out_paths_that_only_a_merged_upstream_changed() {
        let (_guard, tmp) = repo();
        let thread = ThreadId::new("checkpoint-authored-paths");
        let from = checkpoint_ref(thread.as_str(), 0);
        let to = checkpoint_ref(thread.as_str(), 1);
        let commit_before_turn = |file: &str, contents: &str| {
            fs::write(tmp.join(file), contents).unwrap();
            git(&tmp, &["add", "."]);
            git_env(
                &tmp,
                &["commit", "-m", file],
                &[("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")],
            );
        };
        git(&tmp, &["checkout", "-b", "upstream"]);
        commit_before_turn("upstream.txt", "upstream\n");
        commit_before_turn("shared.txt", "upstream\n");
        git(&tmp, &["checkout", "-"]);
        commit_before_turn("shared.txt", "ours\n");
        capture(&tmp, &from);
        capture(&tmp, &to);
        assert_eq!(authored(&tmp, &from, &to), None);

        let _merge = command(&tmp, &["merge", "upstream", "--no-edit"])
            .output()
            .unwrap();
        fs::write(tmp.join("shared.txt"), "upstream\n").unwrap();
        git(&tmp, &["commit", "-am", "merge upstream"]);
        fs::write(tmp.join("README.md"), "# edited\n").unwrap();
        capture(&tmp, &to);
        let paths: Vec<String> =
            parse_turn_diff_files_from_numstat(&numstat(&tmp, &from, &to, false))
                .into_iter()
                .map(|file| file.path)
                .collect();
        assert_eq!(paths, vec!["README.md", "shared.txt", "upstream.txt"]);
        assert_eq!(
            authored(&tmp, &from, &to),
            Some(
                ["README.md".to_string(), "shared.txt".to_string()]
                    .into_iter()
                    .collect()
            )
        );
    }

    #[test]
    fn leaves_out_fetched_commits_newer_than_the_checkpoint_keeping_local_ones() {
        let root = tempfile::tempdir().unwrap();
        let upstream = root.path().join("upstream");
        let primary = root.path().join("primary");
        let cwd = root.path().join("turn");
        fs::create_dir_all(&upstream).unwrap();
        git(&upstream, &["init"]);
        git(&upstream, &["config", "user.email", "test@test.com"]);
        git(&upstream, &["config", "user.name", "Test"]);
        fs::write(upstream.join("README.md"), "# test\n").unwrap();
        git(&upstream, &["add", "."]);
        git(&upstream, &["commit", "-m", "initial commit"]);
        git(root.path(), &["clone", "upstream", "primary"]);
        git(&primary, &["config", "user.email", "test@test.com"]);
        git(&primary, &["config", "user.name", "Test"]);
        git(
            &primary,
            &["worktree", "add", cwd.to_str().unwrap(), "-b", "turn"],
        );
        let thread = ThreadId::new("checkpoint-authored-paths-fetched");
        let from = checkpoint_ref(thread.as_str(), 0);
        let to = checkpoint_ref(thread.as_str(), 1);
        capture(&cwd, &from);

        let commit_file = |repo: &Path, file: &str| {
            fs::write(repo.join(file), format!("{file}\n")).unwrap();
            git(repo, &["add", "."]);
            git(repo, &["commit", "-m", file]);
        };
        git(&upstream, &["checkout", "-b", "release"]);
        commit_file(&upstream, "first.txt");
        git(&cwd, &["fetch", "origin"]);
        commit_file(&upstream, "second.txt");
        git(&primary, &["checkout", "--detach"]);
        commit_file(&primary, "own.txt");
        git(&primary, &["push", "origin", "HEAD:refs/heads/pushed"]);
        git(&upstream, &["merge", "pushed", "--no-edit"]);
        git(&cwd, &["fetch", "origin"]);
        git(&cwd, &["merge", "--ff-only", "origin/release"]);
        commit_file(&cwd, "local.txt");
        git(&cwd, &["push", "origin", "HEAD:refs/heads/mirror"]);
        capture(&cwd, &to);
        assert_eq!(
            authored(&cwd, &from, &to),
            Some(
                ["local.txt".to_string(), "own.txt".to_string()]
                    .into_iter()
                    .collect()
            )
        );
    }

    #[test]
    fn starts_a_turn_that_switched_branches_from_the_branch_it_landed_on() {
        for committed in ["before the turn", "during the turn"] {
            let (_guard, tmp) = repo();
            let thread = ThreadId::new("checkpoint-branch-switch");
            let first = checkpoint_ref(thread.as_str(), 0);
            let second = checkpoint_ref(thread.as_str(), 1);
            let third = checkpoint_ref(thread.as_str(), 2);
            git(&tmp, &["branch", "b"]);
            git(&tmp, &["checkout", "-b", "a"]);
            fs::write(tmp.join("a.txt"), "a\n").unwrap();
            fs::write(tmp.join("README.md"), "# on a\n").unwrap();
            git(&tmp, &["add", "."]);
            if committed == "before the turn" {
                git(&tmp, &["commit", "-am", "work on a"]);
            }
            capture(&tmp, &first);
            if committed == "during the turn" {
                git(&tmp, &["commit", "-am", "work on a"]);
            }
            git(&tmp, &["checkout", "b"]);
            fs::write(tmp.join("README.md"), "# on b\n").unwrap();
            fs::write(tmp.join("b.txt"), "b\n").unwrap();
            capture_after(&tmp, &second, &first);

            let patch = diff(&tmp, &first, &second, false);
            assert!(!patch.contains("a.txt"));
            assert!(patch.contains("-# test\n+# on b\n"));
            assert!(patch.contains("b/b.txt"));
            assert_eq!(authored(&tmp, &first, &second), None);

            git(&tmp, &["add", "."]);
            git(&tmp, &["commit", "-m", "work on b"]);
            capture_after(&tmp, &second, &first);
            git(&tmp, &["reset", "--hard", "HEAD~1"]);
            capture_after(&tmp, &third, &second);
            let reset = diff(&tmp, &second, &third, false);
            assert!(reset.contains("--- a/b.txt\n+++ /dev/null"));
            assert_eq!(
                authored(&tmp, &second, &third),
                Some(
                    ["README.md".to_string(), "b.txt".to_string()]
                        .into_iter()
                        .collect()
                )
            );
        }
    }

    #[test]
    fn keeps_paths_from_signed_commits_under_log_show_signature() {
        let (_guard, tmp) = repo();
        let key_dir = tempfile::tempdir().unwrap();
        let signing_key = key_dir.path().join("key");
        let output = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&signing_key)
            .output()
            .expect("ssh-keygen runs");
        assert!(output.status.success());
        git(&tmp, &["config", "gpg.format", "ssh"]);
        git(
            &tmp,
            &[
                "config",
                "user.signingkey",
                &format!("{}.pub", signing_key.display()),
            ],
        );
        git(&tmp, &["config", "log.showSignature", "true"]);
        let thread = ThreadId::new("checkpoint-authored-paths-signed");
        let from = checkpoint_ref(thread.as_str(), 0);
        let to = checkpoint_ref(thread.as_str(), 1);
        capture(&tmp, &from);
        fs::write(tmp.join("kept.txt"), "kept\n").unwrap();
        git(&tmp, &["add", "kept.txt"]);
        git(&tmp, &["commit", "-S", "-m", "kept"]);
        capture(&tmp, &to);
        assert_eq!(
            authored(&tmp, &from, &to),
            Some(["kept.txt".to_string()].into_iter().collect())
        );
    }

    use super::super::diffs::parse_turn_diff_files_from_numstat;
}
