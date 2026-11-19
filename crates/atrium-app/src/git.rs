//! Blocking Git primitives for the scheduler. Serialize operations for a repository and
//! call them off the UI thread, with the affected agents stopped or between tasks.

use anyhow::{Context, Result, bail, ensure};
use atrium_project::{Project, glob_match};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

fn command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).stdin(Stdio::null());
    // These can redirect -C to another repository or index (e.g. when run from a hook).
    for key in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_PREFIX",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_NAMESPACE",
        "GIT_QUARANTINE_PATH",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("LC_ALL", "C").env("GIT_TERMINAL_PROMPT", "0");
    #[cfg(test)]
    cmd.env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_CONFIG_SYSTEM")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        );
    cmd
}

fn run(dir: &Path, args: &[&OsStr]) -> Result<Output> {
    command(dir)
        .args(args)
        .output()
        .with_context(|| format!("running git {:?} in {}", args.first(), dir.display()))
}

fn output(dir: &Path, args: &[&str]) -> Result<Output> {
    run(dir, &args.iter().map(OsStr::new).collect::<Vec<_>>())
}

fn checked(out: Output, operation: &str) -> Result<Vec<u8>> {
    ensure!(
        out.status.success(),
        "git {operation} ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(out.stdout)
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    checked(output(dir, args)?, args[0])
}

fn success_or_one(out: Output, operation: &str) -> Result<bool> {
    if out.status.code() == Some(1) && out.stderr.is_empty() {
        return Ok(false);
    }
    checked(out, operation)?;
    Ok(true)
}

fn line(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    if cfg!(windows) {
        bytes.strip_suffix(b"\r").unwrap_or(bytes)
    } else {
        bytes
    }
}

fn paths(bytes: &[u8]) -> impl Iterator<Item = Result<PathBuf>> + '_ {
    bytes
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(git_path)
}

fn git_path(bytes: &[u8]) -> Result<PathBuf> {
    #[cfg(unix)]
    let value = <OsString as std::os::unix::ffi::OsStringExt>::from_vec(bytes.to_vec());
    #[cfg(not(unix))]
    let value = OsString::from(std::str::from_utf8(bytes).context("Git path is not UTF-8")?);
    Ok(value.into())
}

fn git_dir(dir: &Path, option: &str) -> Result<PathBuf> {
    git_path(line(&git(
        dir,
        &["rev-parse", "--path-format=absolute", option],
    )?))
}

fn top_level(dir: &Path) -> Result<Option<PathBuf>> {
    let out = output(dir, &["rev-parse", "--show-toplevel"])?;
    if out.status.code() == Some(128)
        && String::from_utf8_lossy(&out.stderr).starts_with("fatal: not a git repository")
    {
        return Ok(None);
    }
    Ok(Some(git_path(line(&checked(
        out,
        "rev-parse --show-toplevel",
    )?))?))
}

fn branch(id: &str) -> Result<String> {
    ensure!(
        !id.is_empty() && id.chars().all(|c| c.is_alphanumeric() || c == '-'),
        "invalid workstream ID: {id:?}"
    );
    if cfg!(windows) {
        let name = id.to_ascii_uppercase();
        ensure!(
            !["CON", "PRN", "AUX", "NUL"].contains(&name.as_str())
                && !["COM", "LPT"]
                    .iter()
                    .any(|p| name.strip_prefix(p).is_some_and(|n| matches!(
                        n,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    ))),
            "workstream ID is a reserved Windows name: {id:?}"
        );
    }
    Ok(Project::branch_of(id))
}

fn head(dir: &Path) -> Result<String> {
    let bytes = git(dir, &["symbolic-ref", "--quiet", "HEAD"])
        .context("an attached local branch is required")?;
    let name = std::str::from_utf8(line(&bytes))?;
    ensure!(
        name.starts_with("refs/heads/"),
        "HEAD is not a local branch: {name}"
    );
    Ok(name.to_owned())
}

fn commit_id(dir: &Path, reference: &str) -> Result<String> {
    let bytes = git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
    )?;
    Ok(std::str::from_utf8(line(&bytes))?.to_owned())
}

fn metadata(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn is_link(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    meta.file_type().is_symlink()
}

fn require_dir(path: &Path, create: bool) -> Result<bool> {
    if let Some(meta) = metadata(path)? {
        ensure!(
            meta.is_dir() && !is_link(&meta),
            "not a plain directory: {}",
            path.display()
        );
        return Ok(true);
    }
    if create {
        fs::create_dir(path).with_context(|| format!("creating {}", path.display()))?;
    }
    Ok(create)
}

fn exclude_atrium(root: &Path) -> Result<()> {
    // The common exclude also applies to linked worktrees and excludes the link itself.
    let exclude = git_dir(root, "--git-common-dir")?
        .join("info")
        .join("exclude");
    fs::create_dir_all(exclude.parent().unwrap())?;
    let bytes = match fs::read(&exclude) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error).with_context(|| format!("reading {}", exclude.display())),
    };
    if !bytes
        .split(|&b| b == b'\n')
        .any(|l| l.strip_suffix(b"\r").unwrap_or(l) == b"/.atrium")
    {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&exclude)?;
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            file.write_all(b"\n")?;
        }
        file.write_all(b"/.atrium\n")?;
    }
    Ok(())
}

fn ignored(dir: &Path, path: &Path) -> Result<bool> {
    success_or_one(
        run(
            dir,
            &[
                OsStr::new("check-ignore"),
                OsStr::new("-q"),
                OsStr::new("--"),
                path.as_os_str(),
            ],
        )?,
        "check-ignore",
    )
}

fn check_atrium(dir: &Path) -> Result<()> {
    ensure!(
        git(dir, &["ls-files", "--", ":(top,icase).atrium"])?.is_empty(),
        "tracked .atrium paths in {}",
        dir.display()
    );
    ensure!(
        ignored(dir, Path::new(".atrium"))?,
        ".atrium is not ignored in {}",
        dir.display()
    );
    Ok(())
}

fn check_tree(root: &Path, tree: &Path, branch: &str) -> Result<()> {
    ensure!(
        require_dir(tree, false)?,
        "missing worktree: {}",
        tree.display()
    );
    let common = git_dir(tree, "--git-common-dir")?.canonicalize()?;
    ensure!(
        common == git_dir(root, "--git-common-dir")?.canonicalize()?
            && git_dir(tree, "--absolute-git-dir")?.canonicalize()? != common
            && top_level(tree)?
                .context("missing worktree")?
                .canonicalize()?
                == tree.canonicalize()?
            && head(tree)? == format!("refs/heads/{branch}"),
        "{} is not the linked worktree for {branch}",
        tree.display()
    );
    Ok(())
}

fn shared_link(tree: &Path, shared: &Path, create: bool) -> Result<()> {
    let link = tree.join(".atrium");
    if let Some(meta) = metadata(&link)? {
        ensure!(
            meta.file_type().is_symlink() && link.canonicalize()? == shared.canonicalize()?,
            "{} does not link to {}",
            link.display(),
            shared.display()
        );
    } else if create {
        let target = Path::new("../..");
        #[cfg(unix)]
        let result = std::os::unix::fs::symlink(target, &link);
        #[cfg(windows)]
        let result = std::os::windows::fs::symlink_dir(target, &link);
        result.with_context(|| {
            format!(
                "linking {} to {}{}",
                link.display(),
                shared.display(),
                if cfg!(windows) {
                    " (Windows needs permission to create directory symlinks)"
                } else {
                    ""
                }
            )
        })?;
    }
    Ok(())
}

fn include_patterns(root: &Path) -> Result<Vec<String>> {
    let path = root.join(".worktreeinclude");
    let Some(meta) = metadata(&path)? else {
        return Ok(Vec::new());
    };
    ensure!(
        meta.is_file() && !is_link(&meta),
        "{} is not a regular file",
        path.display()
    );
    Ok(fs::read_to_string(&path)?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

fn included(patterns: &[String], path: &Path) -> bool {
    // Git emits slash-delimited paths on both platforms, and project globs use slashes.
    path.to_str()
        .is_some_and(|p| patterns.iter().any(|pattern| glob_match(pattern, p)))
}

fn ignored_files(dir: &Path) -> Result<Vec<PathBuf>> {
    paths(&git(
        dir,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--",
            ".",
            ":(top,exclude,icase).atrium",
        ],
    )?)
    .collect()
}

fn plain_parents(root: &Path, path: &Path, create: bool) -> Result<bool> {
    let mut dir = root.to_path_buf();
    for component in path.parent().unwrap().components() {
        let std::path::Component::Normal(name) = component else {
            bail!("invalid Git path: {path:?}")
        };
        dir.push(name);
        if !require_dir(&dir, create)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn copy_includes(root: &Path, tree: &Path) -> Result<()> {
    let patterns = include_patterns(root)?;
    if patterns.is_empty() {
        return Ok(());
    }
    for path in ignored_files(root)?
        .into_iter()
        .filter(|p| included(&patterns, p))
    {
        if !plain_parents(root, &path, false)? {
            continue;
        }
        let src = root.join(&path);
        let Some(meta) = metadata(&src)? else {
            continue;
        };
        if !meta.is_file() || is_link(&meta) {
            continue;
        }
        plain_parents(tree, &path, true)?;
        let dest = tree.join(&path);
        if metadata(&dest)?.is_some() {
            continue;
        }
        ensure!(
            ignored(tree, &path)?,
            "include {:?} is not ignored in {}",
            path,
            tree.display()
        );
        let mut src = File::open(src)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(meta.permissions().mode() & 0o777);
        }
        let mut out = options.open(&dest)?;
        if let Err(error) = io::copy(&mut src, &mut out) {
            drop(out);
            fs::remove_file(&dest).with_context(|| {
                format!("removing partial copy {} after {error}", dest.display())
            })?;
            return Err(error).with_context(|| format!("copying {}", dest.display()));
        }
    }
    Ok(())
}

/// `None` means a non-Git project. For a lead pass `None` for `parent`, and
/// for a worker pass its lead's ID. The project root must be a checkout root.
/// A successfully registered tree stays in place if later setup fails, so a retry
/// can complete setup without resetting the branch or overwriting existing files.
pub(crate) fn ensure_worktree(
    root: &Path,
    id: &str,
    parent: Option<&str>,
) -> Result<Option<PathBuf>> {
    let root = std::path::absolute(root)?;
    let Some(top) = top_level(&root)? else {
        return Ok(None);
    };
    ensure!(
        top.canonicalize()? == root.canonicalize()?,
        "{} is not the Git checkout root",
        root.display()
    );
    let name = branch(id)?;
    let shared = root.join(".atrium");
    let worktrees = shared.join("worktrees");
    let tree = worktrees.join(id);
    require_dir(&shared, true)?;
    require_dir(&worktrees, true)?;
    exclude_atrium(&root)?;
    check_atrium(&root)?;
    if metadata(&tree)?.is_none() {
        let reference = format!("refs/heads/{name}");
        let exists = success_or_one(
            output(&root, &["show-ref", "--verify", "--quiet", &reference])?,
            "show-ref",
        )?;
        let out = if exists {
            run(
                &root,
                &[
                    "worktree".as_ref(),
                    "add".as_ref(),
                    "--".as_ref(),
                    tree.as_os_str(),
                    name.as_ref(),
                ],
            )?
        } else {
            let base = match parent {
                Some(id) => format!("refs/heads/{}", branch(id)?),
                None => head(&root)?,
            };
            let base = commit_id(&root, &base)?;
            run(
                &root,
                &[
                    "worktree".as_ref(),
                    "add".as_ref(),
                    "--no-track".as_ref(),
                    "-b".as_ref(),
                    name.as_ref(),
                    "--".as_ref(),
                    tree.as_os_str(),
                    base.as_ref(),
                ],
            )?
        };
        checked(out, "worktree add")?;
    }
    check_tree(&root, &tree, &name)?;
    check_atrium(&tree)?;
    shared_link(&tree, &shared, true)?;
    copy_includes(&root, &tree)?;
    Ok(Some(tree))
}

fn idle(dir: &Path) -> Result<()> {
    let admin = git_dir(dir, "--absolute-git-dir")?;
    for entry in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-apply",
        "rebase-merge",
        "sequencer",
        "BISECT_START",
    ] {
        ensure!(
            metadata(&admin.join(entry))?.is_none(),
            "Git operation already in progress in {}",
            dir.display()
        );
    }
    Ok(())
}

fn clean(dir: &Path) -> Result<bool> {
    Ok(git(
        dir,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?
    .is_empty())
}

/// Stages a worker's task changes, excluding .atrium. False means no changes to commit.
pub(crate) fn commit_task(tree: &Path, message: &str) -> Result<bool> {
    ensure!(
        head(tree)?.starts_with("refs/heads/atrium/"),
        "expected an Atrium branch"
    );
    idle(tree)?;
    check_atrium(tree)?;
    git(tree, &["add", "-A"])?;
    if success_or_one(
        output(tree, &["diff", "--cached", "--quiet", "HEAD", "--"])?,
        "diff --cached",
    )? {
        return Ok(false);
    }
    git(tree, &["commit", "-m", message])?;
    Ok(true)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Integration {
    Applied,
    Dirty,
    Conflicts(Vec<PathBuf>),
}

fn integrate(dir: &Path, args: &[&str], state: &str) -> Result<Integration> {
    let operation = args[0];
    let out = output(dir, args)?;
    if out.status.success() {
        return Ok(Integration::Applied);
    }
    let error = checked(out, operation).unwrap_err();
    if metadata(&git_dir(dir, "--absolute-git-dir")?.join(state))?.is_none() {
        return Err(error);
    }
    let conflicts = git(dir, &["diff", "--name-only", "--diff-filter=U", "-z"])
        .and_then(|bytes| paths(&bytes).collect::<Result<Vec<_>>>());
    git(dir, &[operation, "--abort"])
        .with_context(|| format!("{error}; automatic abort failed"))?;
    let conflicts = conflicts?;
    if conflicts.is_empty() {
        return Err(error);
    }
    Ok(Integration::Conflicts(conflicts))
}

/// Merges an Atrium branch into the receiving worktree's current branch.
/// A dirty receiver is left alone, and conflicts are returned after a successful abort.
pub(crate) fn merge_into(receiver: &Path, source: &str) -> Result<Integration> {
    head(receiver)?;
    idle(receiver)?;
    if !clean(receiver)? {
        return Ok(Integration::Dirty);
    }
    let source = commit_id(receiver, &format!("refs/heads/{}", branch(source)?))?;
    if overlaps_ignored(receiver, &source, false)? {
        return Ok(Integration::Dirty);
    }
    integrate(
        receiver,
        &[
            "merge",
            "--no-ff",
            "--no-edit",
            "--no-autostash",
            "--no-overwrite-ignore",
            &source,
        ],
        "MERGE_HEAD",
    )
}

/// Rebases a worker onto its lead. Disables auto-stash and updating other refs.
pub(crate) fn rebase_onto(worker: &Path, lead: &str) -> Result<Integration> {
    ensure!(
        head(worker)?.starts_with("refs/heads/atrium/"),
        "expected an Atrium branch"
    );
    idle(worker)?;
    if !clean(worker)? {
        return Ok(Integration::Dirty);
    }
    let base = commit_id(worker, &format!("refs/heads/{}", branch(lead)?))?;
    if overlaps_ignored(worker, &base, true)? {
        return Ok(Integration::Dirty);
    }
    integrate(
        worker,
        &[
            "rebase",
            "--merge",
            "--no-autostash",
            "--no-update-refs",
            "--no-fork-point",
            &base,
        ],
        "rebase-merge",
    )
}

// Git can overwrite ignored files in a non-fast-forward merge or rebase.
fn overlaps_ignored(dir: &Path, base: &str, replay: bool) -> Result<bool> {
    let mut local = ignored_files(dir)?;
    if metadata(&dir.join(".atrium"))?.is_some() {
        local.push(PathBuf::from(".atrium"));
    }
    if local.is_empty() {
        return Ok(false);
    }
    let incoming = git(dir, &["ls-tree", "-r", "--name-only", "-z", base])?;
    let replay = if replay {
        git(
            dir,
            &[
                "log",
                "--format=",
                "--name-only",
                "--no-renames",
                "-z",
                &format!("{base}..HEAD"),
            ],
        )?
    } else {
        Vec::new()
    };
    let local: Vec<_> = local.iter().map(|path| path_key(path)).collect();
    for path in paths(&incoming).chain(paths(&replay)) {
        let name = path_key(&path?);
        if local.iter().any(|local| paths_overlap(local, &name)) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn path_key(path: &Path) -> Vec<OsString> {
    path.components()
        .map(|part| match part.as_os_str().to_str() {
            Some(s) => OsString::from(s.to_lowercase()),
            None => part.as_os_str().to_owned(),
        })
        .collect()
}

fn paths_overlap(a: &[OsString], b: &[OsString]) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Cleanup {
    Removed,
    Dirty,
    Unmerged,
    Ignored(PathBuf),
}

/// Removes a clean worktree whose branch is reachable from the receiving checkout.
/// Pass `delete_worker_branch` only for a retired worker. Lead refs are kept.
pub(crate) fn remove_worktree(
    root: &Path,
    id: &str,
    receiver: &Path,
    delete_worker_branch: bool,
) -> Result<Cleanup> {
    let root = &std::path::absolute(root)?;
    let name = branch(id)?;
    let shared = root.join(".atrium");
    let worktrees = shared.join("worktrees");
    let tree = worktrees.join(id);
    require_dir(&shared, false)?;
    require_dir(&worktrees, false)?;
    check_tree(root, &tree, &name)?;
    ensure!(
        git_dir(root, "--git-common-dir")?.canonicalize()?
            == git_dir(receiver, "--git-common-dir")?.canonicalize()?,
        "cleanup receiver belongs to another repository"
    );
    idle(&tree)?;
    if !clean(&tree)? {
        return Ok(Cleanup::Dirty);
    }
    let tip = commit_id(&tree, "HEAD")?;
    let target = head(receiver)?;
    ensure!(
        target != format!("refs/heads/{name}"),
        "cleanup receiver is the worktree being removed"
    );
    if !success_or_one(
        output(receiver, &["merge-base", "--is-ancestor", &tip, &target])?,
        "merge-base --is-ancestor",
    )? {
        return Ok(Cleanup::Unmerged);
    }
    shared_link(&tree, &shared, false)?;
    let patterns = include_patterns(root)?;
    for path in ignored_files(&tree)? {
        if !included(&patterns, &path)
            || !plain_parents(root, &path, false)?
            || !same_file_contents(&root.join(&path), &tree.join(&path))?
        {
            return Ok(Cleanup::Ignored(path));
        }
    }
    checked(
        run(
            root,
            &[
                "worktree".as_ref(),
                "remove".as_ref(),
                "--".as_ref(),
                tree.as_os_str(),
            ],
        )?,
        "worktree remove",
    )?;
    if delete_worker_branch {
        // -d uses Git's own reachability and checked-out-branch checks as well.
        git(receiver, &["branch", "-d", "--", &name])
            .context("worktree removed, worker branch retained")?;
    }
    Ok(Cleanup::Removed)
}

fn same_file_contents(a: &Path, b: &Path) -> Result<bool> {
    for path in [a, b] {
        if !metadata(path)?.is_some_and(|meta| meta.is_file() && !is_link(&meta)) {
            return Ok(false);
        }
    }
    use std::io::{BufRead, BufReader};
    let mut a = BufReader::new(File::open(a)?);
    let mut b = BufReader::new(File::open(b)?);
    loop {
        let left = a.fill_buf()?;
        let right = b.fill_buf()?;
        let n = left.len().min(right.len());
        if n == 0 {
            return Ok(left.is_empty() && right.is_empty());
        }
        if left[..n] != right[..n] {
            return Ok(false);
        }
        a.consume(n);
        b.consume(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Repo(PathBuf);

    impl Repo {
        fn empty() -> Self {
            let dir =
                std::env::temp_dir().join(format!("Atrium Git 測試 {}", uuid::Uuid::new_v4()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        fn new() -> Self {
            let repo = Self::empty();
            let dir = &repo.0;
            git(dir, &["init", "--initial-branch=main", "--template="]).unwrap();
            for (key, value) in [
                ("user.name", "Atrium Tests"),
                ("user.email", "atrium@example.test"),
                ("commit.gpgSign", "false"),
                ("core.autocrlf", "false"),
            ] {
                git(dir, &["config", "--local", key, value]).unwrap();
            }
            fs::write(dir.join("shared.txt"), "base\n").unwrap();
            fs::write(dir.join(".gitignore"), ".env\nprivate/\n").unwrap();
            git(dir, &["add", "--all"]).unwrap();
            git(dir, &["commit", "-m", "base"]).unwrap();
            repo
        }

        fn tree(&self, id: &str, parent: Option<&str>) -> PathBuf {
            ensure_worktree(&self.0, id, parent).unwrap().unwrap()
        }
    }

    impl Drop for Repo {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.0) {
                eprintln!("cleaning test repository {}: {error}", self.0.display());
            }
        }
    }

    #[test]
    fn creation_reuses_checkout_and_branch_and_preserves_dirty_root() {
        let repo = Repo::new();
        let root = &repo.0;
        let original = commit_id(root, "HEAD").unwrap();
        fs::write(root.join("shared.txt"), "dirty tracked\n").unwrap();
        fs::write(root.join("untracked"), "keep\n").unwrap();
        fs::write(
            root.join(".worktreeinclude"),
            "# local config\r\n.env\r\nprivate/**/config?.txt\r\nshared.txt\r\n",
        )
        .unwrap();
        fs::write(root.join(".env"), "root config\n").unwrap();
        fs::create_dir_all(root.join("private/nested folder")).unwrap();
        fs::write(root.join("private/nested folder/config1.txt"), "copy").unwrap();
        fs::write(root.join("private/nested folder/other.txt"), "skip").unwrap();
        let status = git(root, &["status", "--porcelain=v1"]).unwrap();
        let lead = repo.tree("lead", None);
        assert_eq!(commit_id(&lead, "HEAD").unwrap(), original);
        assert_eq!(
            fs::read_to_string(lead.join("shared.txt")).unwrap(),
            "base\n"
        );
        assert_eq!(
            lead.join(".atrium").canonicalize().unwrap(),
            root.join(".atrium").canonicalize().unwrap()
        );
        assert_eq!(
            fs::read_to_string(lead.join("private/nested folder/config1.txt")).unwrap(),
            "copy"
        );
        assert!(!lead.join("private/nested folder/other.txt").exists());
        assert_eq!(
            fs::read_to_string(lead.join(".env")).unwrap(),
            "root config\n"
        );
        fs::write(lead.join(".env"), "local config\n").unwrap();
        fs::write(lead.join("task.txt"), "task\n").unwrap();
        assert_eq!(repo.tree("lead", None), lead);
        assert_eq!(
            fs::read_to_string(lead.join(".env")).unwrap(),
            "local config\n"
        );
        assert_eq!(fs::read_to_string(lead.join("task.txt")).unwrap(), "task\n");
        assert!(commit_task(&lead, "save task").unwrap());
        assert!(!commit_task(&lead, "empty").unwrap());
        let tip = commit_id(&lead, "HEAD").unwrap();
        let worker = repo.tree("worker", Some("lead"));
        assert_eq!(commit_id(&worker, "HEAD").unwrap(), tip);
        git(root, &["worktree", "remove", worker.to_str().unwrap()]).unwrap();
        assert_eq!(
            commit_id(&repo.tree("worker", Some("lead")), "HEAD").unwrap(),
            tip
        );
        let exclude = fs::read_to_string(
            git_dir(&lead, "--git-common-dir")
                .unwrap()
                .join("info/exclude"),
        )
        .unwrap();
        assert_eq!(
            exclude.lines().filter(|line| *line == "/.atrium").count(),
            1
        );
        assert_eq!(git(root, &["status", "--porcelain=v1"]).unwrap(), status);
        assert_eq!(commit_id(root, "HEAD").unwrap(), original);
    }

    #[test]
    fn worker_merge_rebase_and_safe_retirement() {
        let repo = Repo::new();
        let root = &repo.0;
        let lead = repo.tree("lead", None);
        let worker = repo.tree("worker", Some("lead"));
        fs::write(worker.join("task.txt"), "worker output\n").unwrap();
        assert!(commit_task(&worker, "Worker task (lead/example)").unwrap());
        assert_eq!(
            remove_worktree(root, "worker", &lead, true).unwrap(),
            Cleanup::Unmerged
        );
        fs::write(lead.join("pending.txt"), "untracked\n").unwrap();
        assert_eq!(merge_into(&lead, "worker").unwrap(), Integration::Dirty);
        assert_eq!(
            fs::read_to_string(lead.join("pending.txt")).unwrap(),
            "untracked\n"
        );
        fs::remove_file(lead.join("pending.txt")).unwrap();
        assert_eq!(merge_into(&lead, "worker").unwrap(), Integration::Applied);
        assert_eq!(
            git(&lead, &["rev-list", "--parents", "-n1", "HEAD"])
                .unwrap()
                .split(|&b| b == b' ')
                .count(),
            3
        );
        assert!(!root.join("task.txt").exists());
        fs::write(lead.join("next.txt"), "lead output\n").unwrap();
        commit_task(&lead, "next").unwrap();
        assert_eq!(rebase_onto(&worker, "lead").unwrap(), Integration::Applied);
        assert_eq!(
            commit_id(&worker, "HEAD").unwrap(),
            commit_id(&lead, "HEAD").unwrap()
        );
        fs::write(worker.join("shared.txt"), "keep tracked edit\n").unwrap();
        assert_eq!(
            remove_worktree(root, "worker", &lead, true).unwrap(),
            Cleanup::Dirty
        );
        git(&worker, &["restore", "shared.txt"]).unwrap();
        fs::write(worker.join(".env"), "keep ignored edit\n").unwrap();
        assert_eq!(
            remove_worktree(root, "worker", &lead, true).unwrap(),
            Cleanup::Ignored(PathBuf::from(".env"))
        );
        fs::write(root.join(".worktreeinclude"), ".env\n").unwrap();
        fs::write(root.join(".env"), "keep ignored edit\n").unwrap();
        assert_eq!(
            remove_worktree(root, "worker", &lead, true).unwrap(),
            Cleanup::Removed
        );
        assert!(!worker.exists());
        assert!(commit_id(root, "refs/heads/atrium/worker").is_err());
        assert_eq!(merge_into(root, "lead").unwrap(), Integration::Dirty);
        fs::remove_file(root.join(".worktreeinclude")).unwrap();
        assert_eq!(merge_into(root, "lead").unwrap(), Integration::Applied);
        assert_eq!(
            remove_worktree(root, "lead", root, false).unwrap(),
            Cleanup::Removed
        );
        assert!(commit_id(root, "refs/heads/atrium/lead").is_ok());
        assert!(root.join(".atrium/worktrees").is_dir());
    }

    #[test]
    fn conflicts_abort_and_keep_both_branches() {
        let repo = Repo::new();
        let lead = repo.tree("lead", None);
        let worker = repo.tree("worker", Some("lead"));
        let file = "conflict space 測試.txt";
        for (dir, text) in [(&lead, "lead\n"), (&worker, "worker\n")] {
            fs::write(dir.join(file), text).unwrap();
            commit_task(dir, text).unwrap();
        }
        let lead_tip = commit_id(&lead, "HEAD").unwrap();
        let worker_tip = commit_id(&worker, "HEAD").unwrap();
        assert_eq!(
            merge_into(&lead, "worker").unwrap(),
            Integration::Conflicts(vec![PathBuf::from(file)])
        );
        assert_eq!(
            rebase_onto(&worker, "lead").unwrap(),
            Integration::Conflicts(vec![PathBuf::from(file)])
        );
        assert_eq!(head(&worker).unwrap(), "refs/heads/atrium/worker");
        assert_eq!(commit_id(&lead, "HEAD").unwrap(), lead_tip);
        assert_eq!(commit_id(&worker, "HEAD").unwrap(), worker_tip);
        assert!(clean(&lead).unwrap() && clean(&worker).unwrap());
        assert_eq!(fs::read_to_string(lead.join(file)).unwrap(), "lead\n");
        assert_eq!(fs::read_to_string(worker.join(file)).unwrap(), "worker\n");
    }

    #[test]
    fn integration_preserves_ignored_files_and_existing_git_operations() {
        let repo = Repo::new();
        let lead = repo.tree("lead", None);
        let worker = repo.tree("worker", Some("lead"));
        fs::write(lead.join(".env"), "tracked now\n").unwrap();
        git(&lead, &["add", "-f", ".env"]).unwrap();
        commit_task(&lead, "track config").unwrap();
        fs::write(worker.join(".env"), "private\n").unwrap();
        assert_eq!(rebase_onto(&worker, "lead").unwrap(), Integration::Dirty);
        assert_eq!(merge_into(&worker, "lead").unwrap(), Integration::Dirty);
        assert_eq!(
            fs::read_to_string(worker.join(".env")).unwrap(),
            "private\n"
        );
        assert!(clean(&worker).unwrap());
        fs::remove_file(worker.join(".env")).unwrap();
        git(
            &worker,
            &["merge", "--no-ff", "--no-commit", "refs/heads/atrium/lead"],
        )
        .unwrap();
        assert!(rebase_onto(&worker, "lead").is_err());
        assert!(merge_into(&worker, "lead").is_err());
        let admin = git_dir(&worker, "--absolute-git-dir").unwrap();
        assert!(admin.join("MERGE_HEAD").is_file());
    }

    #[test]
    fn non_git_and_detached_roots() {
        let dir = Repo::empty();
        assert!(ensure_worktree(&dir.0, "lead", None).unwrap().is_none());
        assert!(!dir.0.join(".atrium").exists());
        let repo = Repo::new();
        assert!(ensure_worktree(&repo.0, "../outside", None).is_err());
        git(&repo.0, &["switch", "--detach"]).unwrap();
        assert!(ensure_worktree(&repo.0, "lead", None).is_err());
        assert!(commit_id(&repo.0, "refs/heads/atrium/lead").is_err());
    }

    #[test]
    fn linked_project_uses_common_exclude_and_keeps_foreign_path() {
        let repo = Repo::new();
        let dir = Repo::empty();
        let linked = dir.0.join("linked root");
        git(
            &repo.0,
            &["worktree", "add", "-b", "project", linked.to_str().unwrap()],
        )
        .unwrap();
        let path = linked.join(".atrium/worktrees/lead");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("keep"), "user directory\n").unwrap();
        assert!(ensure_worktree(&linked, "lead", None).is_err());
        assert!(path.join("keep").is_file());
        assert!(commit_id(&linked, "refs/heads/atrium/lead").is_err());
        fs::remove_file(path.join("keep")).unwrap();
        fs::remove_dir(&path).unwrap();
        assert_eq!(
            ensure_worktree(&linked, "lead", None).unwrap().unwrap(),
            path
        );
        assert!(ignored(&path, Path::new(".atrium")).unwrap());
        assert!(ignored(&linked, Path::new(".atrium")).unwrap());
    }
}
