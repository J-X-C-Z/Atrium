//! Contained file reads for a run: a path inside the run folder only, with a
//! byte cap, and — for a file that grows, such as an agent's transcript —
//! complete lines from a cursor.
//!
//! Ported from T3 Code v0.1.11:
//! `apps/server/src/orchestration-v2/workflowFileRead.ts`.

use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;

/// Which file a cursor was reading. A different file (or a shrunk one)
/// starts over from byte zero.
/// What a run file read is capped at unless the caller says otherwise.
pub const DEFAULT_BYTE_CAP: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    pub dev: u64,
    pub ino: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub offset: u64,
    pub identity: Option<FileIdentity>,
}

impl Cursor {
    /// Where a growing file starts being read.
    pub fn new() -> Self {
        Self::default()
    }
}

/// What a read may return.
#[derive(Clone, Copy, Debug)]
pub struct ReadOptions<'a> {
    /// The extension the resolved file must have, without the dot.
    pub extension: Option<&'a str>,
    /// At most this many bytes are read. A cursor read stops at the last
    /// complete line within the cap.
    pub byte_cap: usize,
    /// Keep the end of the file instead of its start.
    pub tail: bool,
    /// Read the lines written since this cursor.
    pub from: Option<Cursor>,
}

impl Default for ReadOptions<'_> {
    fn default() -> Self {
        Self {
            extension: None,
            byte_cap: DEFAULT_BYTE_CAP,
            tail: false,
            from: None,
        }
    }
}

/// The bytes read, and where the next read starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainedFile {
    /// The resolved path, for messages.
    pub path: PathBuf,
    pub contents: String,
    /// The cap cut the file short.
    pub truncated: bool,
    pub start_offset: u64,
    pub end_offset: u64,
    pub identity: FileIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FileError {
    #[error("the path is not a file in this run: {0}")]
    InvalidPath(PathBuf),
    #[error("the run folder is not there")]
    RootUnavailable,
    #[error("the file is not there: {0}")]
    NotFound(PathBuf),
    #[error("the path is outside the run folder: {0}")]
    OutsideRoot(PathBuf),
    #[error("{path} must end in .{expected}")]
    WrongExtension { path: PathBuf, expected: String },
    #[error("the path is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    #[error("the file changed while it was read: {0}")]
    ChangedDuringRead(PathBuf),
    #[error("the file cannot be read: {0}")]
    ReadFailed(PathBuf),
}

fn identity(metadata: &fs::Metadata) -> FileIdentity {
    #[cfg(unix)]
    {
        FileIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
        }
    }
    #[cfg(not(unix))]
    {
        // Windows has no stable device/inode pair through std; a shrunk file
        // still resets a cursor, and a replaced one is caught by its size or
        // extension.
        let _ = metadata;
        FileIdentity { dev: 0, ino: 0 }
    }
}

/// Whether `path` resolves inside `root`. Symlinks are followed, so an
/// escape is refused. A file that is not there is not contained.
pub fn is_contained(root: &Path, path: &Path) -> bool {
    let (Ok(root), Ok(path)) = (fs::canonicalize(root), fs::canonicalize(path)) else {
        return false;
    };
    path == root || path.starts_with(&root)
}

/// Reads `path` (absolute, or relative to `root`) only when it resolves
/// inside `root`, is a regular file, has the wanted extension, and fits the
/// cap. With `from`, reads complete lines written since that cursor.
pub fn read_contained_file(
    root: &Path,
    path: &Path,
    options: ReadOptions<'_>,
) -> Result<ContainedFile, FileError> {
    let root = fs::canonicalize(root).map_err(|_| FileError::RootUnavailable)?;
    let given = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let resolved = fs::canonicalize(&given).map_err(|_| FileError::NotFound(given.clone()))?;
    if resolved != root && !resolved.starts_with(&root) {
        return Err(FileError::OutsideRoot(resolved));
    }
    if let Some(expected) = options.extension {
        let wanted = expected.trim_start_matches('.');
        if resolved.extension().and_then(|e| e.to_str()) != Some(wanted) {
            return Err(FileError::WrongExtension {
                path: resolved,
                expected: wanted.to_string(),
            });
        }
    }
    // Check the leaf before opening: opening a FIFO would wait for a writer.
    let path_metadata =
        fs::symlink_metadata(&resolved).map_err(|_| FileError::NotFound(resolved.clone()))?;
    if !path_metadata.is_file() {
        return Err(FileError::NotRegularFile(resolved));
    }
    let mut file = File::open(&resolved).map_err(|_| FileError::ReadFailed(resolved.clone()))?;
    let metadata = file
        .metadata()
        .map_err(|_| FileError::ReadFailed(resolved.clone()))?;
    if !metadata.is_file() {
        return Err(FileError::NotRegularFile(resolved));
    }
    let file_identity = identity(&metadata);
    if file_identity != identity(&path_metadata) {
        return Err(FileError::ChangedDuringRead(resolved));
    }
    let size = metadata.len();
    let cap = options.byte_cap;
    let mut buffer;
    let (start_offset, truncated, complete_lines) = match options.from {
        Some(cursor) => {
            let start = if cursor.identity == Some(file_identity) && cursor.offset <= size {
                cursor.offset
            } else {
                0
            };
            buffer = vec![0u8; (size - start).min(cap as u64) as usize];
            file.seek(SeekFrom::Start(start))
                .map_err(|_| FileError::ReadFailed(resolved.clone()))?;
            let read = file
                .read(&mut buffer)
                .map_err(|_| FileError::ReadFailed(resolved.clone()))?;
            buffer.truncate(read);
            (start, size - start > cap as u64, true)
        }
        None => {
            let length = size.min(cap as u64 + u64::from(options.tail)) as usize;
            buffer = vec![0u8; length];
            let start = if options.tail {
                size - length as u64
            } else {
                0
            };
            file.seek(SeekFrom::Start(start))
                .map_err(|_| FileError::ReadFailed(resolved.clone()))?;
            let read = file
                .read(&mut buffer)
                .map_err(|_| FileError::ReadFailed(resolved.clone()))?;
            buffer.truncate(read);
            (start, size > length as u64, false)
        }
    };
    // A cursor read defers a half-written last line. A single line longer
    // than the cap is consumed whole so the cursor can still advance.
    let line_end = if complete_lines {
        buffer
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |at| at + 1)
    } else {
        buffer.len()
    };
    let consumed = if complete_lines && line_end == 0 && buffer.len() == cap {
        buffer.len()
    } else {
        line_end
    };
    Ok(ContainedFile {
        path: resolved,
        contents: String::from_utf8_lossy(&buffer[..line_end]).into_owned(),
        truncated,
        start_offset,
        end_offset: start_offset + consumed as u64,
        identity: file_identity,
    })
}

/// Reads a whole small file under `root` as text.
pub fn read_text(
    root: &Path,
    path: impl AsRef<Path>,
    byte_cap: usize,
) -> Result<String, FileError> {
    read_contained_file(
        root,
        path.as_ref(),
        ReadOptions {
            byte_cap,
            ..ReadOptions::default()
        },
    )
    .map(|file| file.contents)
}

/// The template a run started with (`.atrium/workflow.toml`), read the way
/// T3 reads a workflow script: under the run folder, capped, never outside.
pub fn read_template_snapshot(run: &Path) -> Result<ContainedFile, FileError> {
    read_contained_file(
        run,
        Path::new(".atrium/workflow.toml"),
        ReadOptions {
            extension: Some("toml"),
            ..ReadOptions::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn read(root: &Path, name: &str) -> Result<ContainedFile, FileError> {
        read_contained_file(
            root,
            Path::new(name),
            ReadOptions {
                extension: Some("js"),
                ..ReadOptions::default()
            },
        )
    }

    #[test]
    fn a_real_script_under_the_root_is_served() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("run.js");
        fs::write(&script, "export const meta = {};\n").unwrap();
        let result = read(dir.path(), "run.js").unwrap();
        assert!(result.contents.contains("export const meta"));
        assert!(!result.truncated);
        assert_eq!(result.end_offset, fs::metadata(&script).unwrap().len());
    }

    #[test]
    fn paths_outside_the_root_and_symlink_escapes_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let outside = outside_dir.path().join("evil.js");
        fs::write(&outside, "evil\n").unwrap();
        assert_eq!(
            read(dir.path(), outside.to_str().unwrap()),
            Err(FileError::OutsideRoot(fs::canonicalize(&outside).unwrap()))
        );
        #[cfg(unix)]
        {
            let link = dir.path().join("sneaky.js");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(matches!(
                read(dir.path(), "sneaky.js"),
                Err(FileError::OutsideRoot(_))
            ));
        }
    }

    #[test]
    fn a_wrong_extension_and_a_directory_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("run.txt"), "text").unwrap();
        assert!(matches!(
            read(dir.path(), "run.txt"),
            Err(FileError::WrongExtension { .. })
        ));
        fs::create_dir_all(dir.path().join("folder.js")).unwrap();
        assert!(matches!(
            read(dir.path(), "folder.js"),
            Err(FileError::NotRegularFile(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_waiting_for_a_writer() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.js");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if !made.is_ok_and(|status| status.success()) {
            return; // No mkfifo here; the leaf check is still covered above.
        }
        assert!(matches!(
            read(dir.path(), "pipe.js"),
            Err(FileError::NotRegularFile(_))
        ));
    }

    #[test]
    fn a_cap_keeps_either_the_start_or_the_end_and_says_it_truncated() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("run.js"), "0123456789").unwrap();
        let start = read_contained_file(
            dir.path(),
            Path::new("run.js"),
            ReadOptions {
                byte_cap: 4,
                ..ReadOptions::default()
            },
        )
        .unwrap();
        assert_eq!(start.contents, "0123");
        assert!(start.truncated);
        assert_eq!(start.end_offset, 4);
        let tail = read_contained_file(
            dir.path(),
            Path::new("run.js"),
            ReadOptions {
                byte_cap: 4,
                tail: true,
                ..ReadOptions::default()
            },
        )
        .unwrap();
        // A tail read keeps one byte over the cap, as T3 does, to tell a
        // truncated file from one that exactly fits.
        assert_eq!(tail.contents, "56789");
        assert_eq!(tail.start_offset, 5);
        assert!(tail.truncated);
    }

    #[test]
    fn the_template_snapshot_is_read_from_the_run_folder() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".atrium")).unwrap();
        fs::write(dir.path().join(".atrium/workflow.toml"), "id = \"x\"").unwrap();
        let snapshot = read_template_snapshot(dir.path()).unwrap();
        assert_eq!(snapshot.contents, "id = \"x\"");
        // The same name outside the run folder is not this run's template.
        let other = tempfile::tempdir().unwrap();
        assert!(read_template_snapshot(other.path()).is_err());
    }

    /// From T3's `workflowAgentTranscript.test.ts`: complete lines only, a
    /// half-written line waits, and a new or shrunk file starts over.
    #[test]
    fn reads_complete_lines_from_its_cursor_and_starts_over_on_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("projects/test");
        fs::create_dir_all(&transcript).unwrap();
        let path = transcript.join("agent-a1.jsonl");
        let read = |cursor: Cursor| {
            read_contained_file(
                dir.path(),
                &path,
                ReadOptions {
                    extension: Some("jsonl"),
                    byte_cap: 512 * 1024,
                    from: Some(cursor),
                    ..ReadOptions::default()
                },
            )
            .unwrap()
        };
        let line = |value: serde_json::Value| format!("{value}\n");
        let assistant = |uuid: &str, text: &str| {
            line(serde_json::json!({
                "type": "assistant",
                "uuid": uuid,
                "message": {"role": "assistant", "id": uuid,
                            "content": [{"type": "text", "text": text}]}
            }))
        };
        let uuids = |lines: &[serde_json::Value]| {
            lines
                .iter()
                .map(|entry| entry["uuid"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };

        let bookkeeping = line(serde_json::json!({
            "type": "attachment", "uuid": "att", "attachment": {"type": "skill_listing"}
        })) + &line(serde_json::json!({
            "type": "user", "uuid": "meta", "isMeta": true, "message": {"content": "context"}
        }));
        let second = assistant("b", "second");
        fs::write(
            &path,
            format!("{bookkeeping}{}{}", assistant("a", "first"), &second[..20]),
        )
        .unwrap();
        let first = read(Cursor::new());
        let lines = crate::answers::parse_lines(&first.contents);
        assert_eq!(uuids(&lines), ["a"]);
        assert_eq!(
            first.end_offset,
            (bookkeeping.len() + assistant("a", "first").len()) as u64,
            "a half-written line is left for the next read"
        );

        let mut writer = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writer.write_all(&second.as_bytes()[20..]).unwrap();
        let next = read(Cursor {
            offset: first.end_offset,
            identity: Some(first.identity),
        });
        assert_eq!(uuids(&crate::answers::parse_lines(&next.contents)), ["b"]);
        assert_eq!(next.end_offset, fs::metadata(&path).unwrap().len());

        fs::remove_file(&path).unwrap();
        fs::write(&path, assistant("c", "replaced")).unwrap();
        let replaced = read(Cursor {
            offset: next.end_offset,
            identity: Some(next.identity),
        });
        assert_eq!(
            uuids(&crate::answers::parse_lines(&replaced.contents)),
            ["c"],
            "a replaced file reads from zero"
        );

        fs::write(&path, assistant("d", "x")).unwrap();
        let shrunk = read(Cursor {
            offset: replaced.end_offset,
            identity: Some(replaced.identity),
        });
        assert_eq!(
            uuids(&crate::answers::parse_lines(&shrunk.contents)),
            ["d"],
            "a shrunk file reads from zero"
        );
    }
}
