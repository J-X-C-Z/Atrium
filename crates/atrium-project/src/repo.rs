//! Git for one conversation's folder: what changed, commit, push, open a
//! pull request, undo a file or a hunk, checkpoints before each turn, and
//! a worktree of its own. Blocking; call off the UI thread.

use anyhow::{Context, Result, bail, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

fn command(program: &str, dir: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.current_dir(dir).stdin(Stdio::null());
    for key in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_PREFIX"] {
        cmd.env_remove(key);
    }
    cmd.env("LC_ALL", "C").env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

fn checked(out: Output, what: &str) -> Result<String> {
    ensure!(
        out.status.success(),
        "{what}: {}",
        first_error(&out).unwrap_or_else(|| out.status.to_string())
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn first_error(out: &Output) -> Option<String> {
    let text = String::from_utf8_lossy(&out.stderr);
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = command("git", dir)
        .args(args)
        .output()
        .context("running git")?;
    checked(out, &format!("git {}", args[0]))
}

/// Runs git with a private index, so the user's staging area is untouched.
fn git_index(dir: &Path, index: &Path, args: &[&str]) -> Result<String> {
    let out = command("git", dir)
        .env("GIT_INDEX_FILE", index)
        .args(args)
        .output()
        .context("running git")?;
    checked(out, &format!("git {}", args[0]))
}

/// The top of the repository `dir` is in, if any.
pub fn root(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    let line = out.trim();
    (!line.is_empty()).then(|| PathBuf::from(line))
}

/// A project identity that survives clones on different machines.
///
/// It is the normalized `origin` URL plus the path inside the repository;
/// when there is no remote, the root commit is used instead. The id is a
/// short hash so it can be an overlay key.
pub fn identity(dir: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};

    let root = root(dir)?;
    let relative = dir
        .canonicalize()
        .ok()
        .and_then(|dir| {
            root.canonicalize()
                .ok()
                .and_then(|root| dir.strip_prefix(root).ok().map(Path::to_path_buf))
        })
        .unwrap_or_default();
    let relative = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let origin = git(&root, &["remote", "get-url", "origin"])
        .ok()
        .map(|url| normalize_origin(url.trim()));
    let base = match origin {
        Some(origin) if !origin.is_empty() => origin,
        _ => git(&root, &["rev-list", "--max-parents=0", "HEAD"])
            .ok()?
            .lines()
            .next()?
            .trim()
            .to_string(),
    };
    let digest = Sha256::digest(format!("{base}|{relative}").as_bytes());
    Some(
        digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

fn normalize_origin(url: &str) -> String {
    let mut url = url.trim().trim_end_matches('/').to_string();
    if let Some(rest) = url.strip_suffix(".git") {
        url = rest.to_string();
    }
    if let Some(rest) = url.strip_prefix("git@")
        && let Some((host, path)) = rest.split_once(':')
    {
        url = format!("{host}/{path}");
    }
    for prefix in ["https://", "http://", "ssh://", "git://"] {
        if let Some(rest) = url.strip_prefix(prefix) {
            url = rest.to_string();
            break;
        }
    }
    if let Some((user, rest)) = url.split_once('@')
        && !user.contains('/')
    {
        url = rest.to_string();
    }
    url.to_ascii_lowercase()
}

fn has_head(root: &Path) -> bool {
    git(root, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok()
}

/// One changed path in the working tree, against `HEAD`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Relative to the repository root, `/`-separated.
    pub path: String,
    pub state: ChangeState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeState {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

/// The branch, what changed and how far ahead of its upstream it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub root: PathBuf,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub changes: Vec<Change>,
}

pub fn status(dir: &Path) -> Result<Status> {
    let root = root(dir).context("not a git repository")?;
    let out = git(
        &root,
        &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all"],
    )?;
    let mut status = Status {
        root: root.clone(),
        ..Status::default()
    };
    let mut fields = out.split('\0').peekable();
    while let Some(entry) = fields.next() {
        if let Some(head) = entry.strip_prefix("# branch.head ") {
            status.branch = (head != "(detached)").then(|| head.to_string());
        } else if let Some(up) = entry.strip_prefix("# branch.upstream ") {
            status.upstream = Some(up.to_string());
        } else if let Some(ab) = entry.strip_prefix("# branch.ab ") {
            let mut parts = ab.split_whitespace();
            status.ahead = parts.next().and_then(|a| a[1..].parse().ok()).unwrap_or(0);
            status.behind = parts.next().and_then(|b| b[1..].parse().ok()).unwrap_or(0);
        } else if let Some(path) = entry.strip_prefix("? ") {
            status.changes.push(Change {
                path: path.to_string(),
                state: ChangeState::Untracked,
            });
        } else if entry.starts_with("1 ") || entry.starts_with("2 ") || entry.starts_with("u ") {
            let renamed = entry.starts_with("2 ");
            let parts: Vec<&str> = entry.splitn(if renamed { 10 } else { 9 }, ' ').collect();
            let xy = parts.get(1).copied().unwrap_or("..");
            let path = parts.last().copied().unwrap_or_default().to_string();
            if renamed {
                // The original path follows as its own field.
                fields.next();
            }
            let state = if entry.starts_with("u ") {
                ChangeState::Conflicted
            } else if renamed {
                ChangeState::Renamed
            } else if xy.contains('D') {
                ChangeState::Deleted
            } else if xy.starts_with('A') {
                ChangeState::Added
            } else {
                ChangeState::Modified
            };
            status.changes.push(Change { path, state });
        }
    }
    Ok(status)
}

/// The unified diff of `path` against `HEAD`, as `git diff` prints it, with
/// an untracked file shown as all added.
pub fn diff(root: &Path, change: &Change) -> Result<String> {
    if change.state == ChangeState::Untracked || !has_head(root) {
        let out = command("git", root)
            .args(["diff", "--no-color", "--no-index", "--", "/dev/null", &change.path])
            .output()
            .context("running git")?;
        // `--no-index` exits 1 when the files differ.
        ensure!(out.status.code() != Some(2), "git diff: {}", first_error(&out).unwrap_or_default());
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    git(root, &["diff", "--no-color", "HEAD", "--", &change.path])
}

/// Puts `path` back as it is in `HEAD`; a file `HEAD` does not have is removed.
pub fn revert_file(root: &Path, change: &Change) -> Result<()> {
    match change.state {
        ChangeState::Untracked => remove(root, &change.path),
        ChangeState::Added => {
            git(root, &["rm", "-q", "--cached", "--", &change.path])?;
            remove(root, &change.path)
        }
        _ => {
            git(root, &["checkout", "-q", "HEAD", "--", &change.path])?;
            Ok(())
        }
    }
}

fn remove(root: &Path, path: &str) -> Result<()> {
    let full = root.join(path);
    if full.is_dir() {
        fs::remove_dir_all(&full)?;
    } else if full.exists() {
        fs::remove_file(&full)?;
    }
    Ok(())
}

/// Splits `git diff` output into its header and its hunks, each hunk starting
/// with its `@@` line.
pub fn hunks(diff: &str) -> (String, Vec<String>) {
    let mut header = String::new();
    let mut hunks: Vec<String> = Vec::new();
    for line in diff.split_inclusive('\n') {
        if line.starts_with("@@") {
            hunks.push(line.to_string());
        } else if let Some(last) = hunks.last_mut() {
            last.push_str(line);
        } else {
            header.push_str(line);
        }
    }
    (header, hunks)
}

/// Undoes hunk `index` of `path`'s diff against `HEAD` in the working tree.
pub fn revert_hunk(root: &Path, change: &Change, index: usize) -> Result<()> {
    let diff = diff(root, change)?;
    let (header, hunks) = hunks(&diff);
    let hunk = hunks.get(index).context("that part changed meanwhile")?;
    if hunks.len() == 1 {
        return revert_file(root, change);
    }
    let patch = format!("{header}{hunk}");
    let dir = tempdir()?;
    let file = dir.join("hunk.patch");
    fs::write(&file, patch)?;
    let result = git(
        root,
        &["apply", "-R", "--whitespace=nowarn", &file.to_string_lossy()],
    );
    let _ = fs::remove_dir_all(&dir);
    result.map(drop)
}

fn tempdir() -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("atrium-git-{}", uuid_like()));
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

/// Stages everything and commits it. Returns the new commit's short id.
pub fn commit_all(root: &Path, message: &str) -> Result<String> {
    ensure!(!message.trim().is_empty(), "write a commit message first");
    git(root, &["add", "-A"])?;
    let dir = tempdir()?;
    let file = dir.join("message");
    fs::write(&file, message.trim())?;
    let result = git(root, &["commit", "-q", "-F", &file.to_string_lossy()]);
    let _ = fs::remove_dir_all(&dir);
    result?;
    Ok(git(root, &["rev-parse", "--short", "HEAD"])?.trim().to_string())
}

/// Pushes the current branch, setting its upstream the first time.
pub fn push(root: &Path) -> Result<()> {
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim();
    ensure!(branch != "HEAD", "not on a branch");
    let remote = git(root, &["remote"])?;
    let remote = remote
        .lines()
        .find(|r| *r == "origin")
        .or_else(|| remote.lines().next())
        .context("this repository has no remote to push to")?
        .to_string();
    git(root, &["push", "-u", &remote, branch])?;
    Ok(())
}

/// Opens a pull request for the current branch with `gh`, filled from its
/// commits. Returns its address.
pub fn create_pr(root: &Path) -> Result<String> {
    let out = command("gh", root)
        .args(["pr", "create", "--fill"])
        .output()
        .context("GitHub CLI (gh) is not installed")?;
    let text = checked(out, "gh pr create")?;
    let url = text
        .lines()
        .rev()
        .find(|l| l.starts_with("https://"))
        .unwrap_or(text.trim())
        .to_string();
    Ok(url)
}

// ---- Checkpoints ---------------------------------------------------------

/// Where the checkpoints of conversation `thread` live.
fn checkpoint_ref(thread: &str, turn: usize) -> String {
    let safe: String = thread
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!("refs/atrium/checkpoints/{safe}/{turn}")
}

/// A tree of the working tree as it is now, untracked files included and
/// ignored ones left out, without touching the user's index.
fn snapshot_tree(root: &Path) -> Result<String> {
    let dir = tempdir()?;
    let index = dir.join("index");
    let result = (|| {
        if has_head(root) {
            git_index(root, &index, &["read-tree", "HEAD"])?;
        }
        git_index(root, &index, &["add", "-A"])?;
        Ok(git_index(root, &index, &["write-tree"])?.trim().to_string())
    })();
    let _ = fs::remove_dir_all(&dir);
    result
}

/// Records the files as they are before turn `turn` of `thread`.
pub fn checkpoint(dir: &Path, thread: &str, turn: usize) -> Result<()> {
    let root = root(dir).context("not a git repository")?;
    let tree = snapshot_tree(&root)?;
    let mut args = vec!["commit-tree", tree.as_str(), "-m"];
    let message = format!("Atrium checkpoint: {thread} before turn {turn}");
    args.push(&message);
    let head = git(&root, &["rev-parse", "-q", "--verify", "HEAD"]).ok();
    if let Some(head) = head.as_deref() {
        args.extend(["-p", head.trim()]);
    }
    let commit = command("git", &root)
        .args(&args)
        .env("GIT_AUTHOR_NAME", "Atrium")
        .env("GIT_AUTHOR_EMAIL", "atrium@localhost")
        .env("GIT_COMMITTER_NAME", "Atrium")
        .env("GIT_COMMITTER_EMAIL", "atrium@localhost")
        .output()
        .context("running git")?;
    let commit = checked(commit, "git commit-tree")?;
    git(&root, &["update-ref", &checkpoint_ref(thread, turn), commit.trim()])?;
    Ok(())
}

/// The turns of `thread` that have a checkpoint.
pub fn checkpoints(dir: &Path, thread: &str) -> Vec<usize> {
    let Some(root) = root(dir) else {
        return Vec::new();
    };
    let prefix = checkpoint_ref(thread, 0);
    let prefix = prefix.trim_end_matches('0');
    let Ok(out) = git(&root, &["for-each-ref", "--format=%(refname)", prefix]) else {
        return Vec::new();
    };
    let mut turns: Vec<usize> = out
        .lines()
        .filter_map(|r| r.strip_prefix(prefix)?.parse().ok())
        .collect();
    turns.sort_unstable();
    turns
}

/// Moves the checkpoints taken under `from` (a chat not started yet) to
/// `to`, the conversation's id once it has one.
pub fn rename_checkpoints(dir: &Path, from: &str, to: &str) -> Result<()> {
    let root = root(dir).context("not a git repository")?;
    for turn in checkpoints(&root, from) {
        let old = checkpoint_ref(from, turn);
        let commit = git(&root, &["rev-parse", &old])?;
        git(&root, &["update-ref", &checkpoint_ref(to, turn), commit.trim()])?;
        git(&root, &["update-ref", "-d", &old])?;
    }
    Ok(())
}

/// Puts every file back as it was before turn `turn` of `thread`: changed
/// and deleted files are restored, files created since are removed. Ignored
/// files are left alone. The files as they are now become checkpoint
/// `undo_turn`, so the restore can itself be undone.
pub fn restore(dir: &Path, thread: &str, turn: usize, undo_turn: usize) -> Result<()> {
    let root = root(dir).context("not a git repository")?;
    let target = git(&root, &["rev-parse", &format!("{}^{{tree}}", checkpoint_ref(thread, turn))])
        .context("no checkpoint for that turn")?;
    let target = target.trim().to_string();
    checkpoint(&root, thread, undo_turn)?;
    let now = snapshot_tree(&root)?;
    if now == target {
        return Ok(());
    }
    let changed = git(&root, &["diff", "--name-status", "--no-renames", "-z", &target, &now])?;
    let mut fields = changed.split('\0').filter(|f| !f.is_empty());
    let mut put_back = Vec::new();
    while let (Some(state), Some(path)) = (fields.next(), fields.next()) {
        if state == "A" {
            remove(&root, path)?;
        } else {
            put_back.push(path.to_string());
        }
    }
    if !put_back.is_empty() {
        let dir = tempdir()?;
        let index = dir.join("index");
        let result = (|| {
            git_index(&root, &index, &["read-tree", &target])?;
            let mut args = vec!["checkout-index", "-f", "--"];
            args.extend(put_back.iter().map(String::as_str));
            git_index(&root, &index, &args)
        })();
        let _ = fs::remove_dir_all(&dir);
        result?;
    }
    Ok(())
}

// ---- Worktrees for chats -------------------------------------------------

/// A new worktree of the repository `dir` is in, on a new branch
/// `atrium/<name>` from the current `HEAD`, under `base`. Returns its folder.
pub fn add_worktree(dir: &Path, base: &Path, name: &str) -> Result<PathBuf> {
    let root = root(dir).context("not a git repository")?;
    ensure!(has_head(&root), "the repository has no commits yet");
    let slug: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() { "chat".to_string() } else { slug };
    let repo = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let mut n = 0;
    let (folder, branch) = loop {
        let tail = if n == 0 { slug.clone() } else { format!("{slug}-{n}") };
        let folder = base.join(&repo).join(&tail);
        let branch = format!("atrium/{tail}");
        let taken = folder.exists()
            || git(&root, &["rev-parse", "-q", "--verify", &format!("refs/heads/{branch}")]).is_ok();
        if !taken {
            break (folder, branch);
        }
        n += 1;
        if n > 99 {
            bail!("no free worktree name for {slug}");
        }
    };
    fs::create_dir_all(folder.parent().unwrap_or(base))?;
    git(
        &root,
        &["worktree", "add", "-q", "-b", &branch, &folder.to_string_lossy(), "HEAD"],
    )?;
    Ok(folder)
}

/// The main checkout of the repository a worktree belongs to, when `dir` is
/// a linked worktree rather than the main one.
pub fn main_checkout(dir: &Path) -> Option<PathBuf> {
    let top = root(dir)?;
    let common = git(&top, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).ok()?;
    let common = PathBuf::from(common.trim());
    let main = common.parent()?.to_path_buf();
    (fs::canonicalize(&main).ok()? != fs::canonicalize(&top).ok()?).then_some(main)
}

/// Merges the worktree's branch into whatever the main checkout has out.
/// Commit or undo the worktree's changes first.
pub fn merge_worktree(dir: &Path) -> Result<String> {
    let top = root(dir).context("not a git repository")?;
    let main = main_checkout(&top).context("this folder is not a separate worktree")?;
    let changes = status(&top)?.changes;
    ensure!(changes.is_empty(), "commit or undo this worktree's changes first");
    ensure!(status(&main)?.changes.is_empty(), "the main checkout has uncommitted changes");
    let branch = git(&top, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let into = git(&main, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let out = command("git", &main)
        .args(["merge", "--no-edit", branch.trim()])
        .output()
        .context("running git")?;
    if !out.status.success() {
        let _ = git(&main, &["merge", "--abort"]);
        bail!(
            "merging {} into {} conflicts; resolve it in a terminal",
            branch.trim(),
            into.trim()
        );
    }
    Ok(into.trim().to_string())
}

/// Removes a chat's worktree folder. Its branch stays.
pub fn remove_worktree(dir: &Path) -> Result<()> {
    let top = root(dir).context("not a git repository")?;
    let main = main_checkout(&top).context("this folder is not a separate worktree")?;
    git(&main, &["worktree", "remove", "--force", &top.to_string_lossy()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> PathBuf {
        let dir = tempdir().unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@example.com"],
            vec!["config", "user.name", "T"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git(&dir, &args).unwrap();
        }
        fs::write(dir.join("a.txt"), "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\ntwelve\n").unwrap();
        fs::write(dir.join("b.txt"), "b\n").unwrap();
        fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        git(&dir, &["add", "-A"]).unwrap();
        git(&dir, &["commit", "-qm", "init"]).unwrap();
        dir
    }

    fn change<'a>(s: &'a Status, path: &str) -> &'a Change {
        s.changes.iter().find(|c| c.path == path).unwrap()
    }

    #[test]
    fn identity_is_stable_and_ignores_the_checkout_path() {
        let dir = repo();
        let first = identity(&dir).unwrap();
        assert_eq!(identity(&dir).unwrap(), first);
        assert_eq!(first.len(), 16);
        let nested = dir.join("nested");
        fs::create_dir_all(&nested).unwrap();
        assert_ne!(identity(&nested).unwrap(), first);
    }

    #[test]
    fn remote_urls_normalize_the_same_way() {
        assert_eq!(
            normalize_origin("git@GitHub.com:owner/repo.git"),
            "github.com/owner/repo"
        );
        assert_eq!(
            normalize_origin("https://github.com/owner/repo/"),
            "github.com/owner/repo"
        );
    }

    #[test]
    fn status_lists_every_kind_of_change() {
        let dir = repo();
        fs::write(dir.join("a.txt"), "changed\n").unwrap();
        fs::remove_file(dir.join("b.txt")).unwrap();
        fs::create_dir_all(dir.join("new")).unwrap();
        fs::write(dir.join("new/c.txt"), "c\n").unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("target/out"), "x").unwrap();
        let s = status(&dir).unwrap();
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(change(&s, "a.txt").state, ChangeState::Modified);
        assert_eq!(change(&s, "b.txt").state, ChangeState::Deleted);
        assert_eq!(change(&s, "new/c.txt").state, ChangeState::Untracked);
        assert_eq!(s.changes.len(), 3, "ignored files stay out");
        assert!(diff(&dir, change(&s, "new/c.txt")).unwrap().contains("+c"));
    }

    #[test]
    fn a_file_or_one_hunk_can_be_undone() {
        let dir = repo();
        let text = "ONE\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\neleven\nTWELVE\n";
        fs::write(dir.join("a.txt"), text).unwrap();
        fs::write(dir.join("new.txt"), "n\n").unwrap();
        let s = status(&dir).unwrap();
        let a = change(&s, "a.txt").clone();
        let (_, parts) = hunks(&diff(&dir, &a).unwrap());
        assert_eq!(parts.len(), 2);
        revert_hunk(&dir, &a, 1).unwrap();
        let now = fs::read_to_string(dir.join("a.txt")).unwrap();
        assert!(now.starts_with("ONE\n") && now.ends_with("twelve\n"));
        revert_file(&dir, &a).unwrap();
        assert!(fs::read_to_string(dir.join("a.txt")).unwrap().starts_with("one\n"));
        revert_file(&dir, change(&s, "new.txt")).unwrap();
        assert!(!dir.join("new.txt").exists());
        assert!(status(&dir).unwrap().changes.is_empty());
    }

    #[test]
    fn commit_takes_everything() {
        let dir = repo();
        fs::write(dir.join("b.txt"), "B\n").unwrap();
        fs::write(dir.join("c.txt"), "c\n").unwrap();
        let id = commit_all(&dir, "Change b, add c").unwrap();
        assert!(!id.is_empty());
        assert!(status(&dir).unwrap().changes.is_empty());
        assert!(commit_all(&dir, "  ").is_err());
    }

    #[test]
    fn checkpoints_restore_and_undo() {
        let dir = repo();
        fs::write(dir.join("b.txt"), "staged\n").unwrap();
        git(&dir, &["add", "b.txt"]).unwrap();
        checkpoint(&dir, "thread/1", 0).unwrap();
        // The turn: edits, deletes, creates; ignored files stay.
        fs::write(dir.join("a.txt"), "agent\n").unwrap();
        fs::remove_file(dir.join("b.txt")).unwrap();
        fs::write(dir.join("made.txt"), "m\n").unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("target/build"), "x").unwrap();
        checkpoint(&dir, "draft", 0).unwrap();
        rename_checkpoints(&dir, "draft", "thread/2").unwrap();
        assert!(checkpoints(&dir, "draft").is_empty());
        assert_eq!(checkpoints(&dir, "thread/2"), vec![0]);
        assert_eq!(checkpoints(&dir, "thread/1"), vec![0]);
        restore(&dir, "thread/1", 0, 1).unwrap();
        assert!(fs::read_to_string(dir.join("a.txt")).unwrap().starts_with("one\n"));
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "staged\n");
        assert!(!dir.join("made.txt").exists());
        assert!(dir.join("target/build").exists());
        // The index the user staged is untouched by checkpoints.
        assert!(git(&dir, &["diff", "--cached", "--name-only"]).unwrap().contains("b.txt"));
        // And the restore can be undone.
        restore(&dir, "thread/1", 1, 2).unwrap();
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "agent\n");
        assert!(dir.join("made.txt").exists());
        assert!(!dir.join("b.txt").exists());
    }

    #[test]
    fn a_chat_worktree_merges_back_and_goes() {
        let dir = repo();
        let base = tempdir().unwrap();
        let tree = add_worktree(&dir, &base, "Fix the Login page!").unwrap();
        assert!(tree.ends_with("fix-the-login-page"));
        assert!(main_checkout(&tree).is_some());
        assert!(main_checkout(&dir).is_none());
        fs::write(tree.join("b.txt"), "from tree\n").unwrap();
        assert!(merge_worktree(&tree).is_err(), "uncommitted work blocks the merge");
        commit_all(&tree, "Tree work").unwrap();
        assert_eq!(merge_worktree(&tree).unwrap(), "main");
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "from tree\n");
        let again = add_worktree(&dir, &base, "Fix the Login page!").unwrap();
        assert!(again.ends_with("fix-the-login-page-1"));
        remove_worktree(&tree).unwrap();
        assert!(!tree.exists());
    }
}
