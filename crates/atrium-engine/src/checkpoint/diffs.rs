// Ported from apps/server/src/checkpointing/Diffs.ts (T3 Code v0.1.11, MIT).
//! Turn diff summaries and the rule that separates a turn's own edits from
//! files a pull, merge or rebase imported.

use std::collections::HashSet;

/// One file in a turn diff, as `git diff --numstat -z` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnDiffFileSummary {
    pub path: String,
    pub previous_path: Option<String>,
    pub additions: u32,
    pub deletions: u32,
}

/// Parses NUL-delimited `--numstat -z` output.
///
/// Renames and copies carry both paths in a separate NUL field, so the record
/// after a `\0` path belongs to the same file.
pub fn parse_turn_diff_files_from_numstat(numstat: &str) -> Vec<TurnDiffFileSummary> {
    let records: Vec<&str> = numstat.split('\0').collect();
    let mut files = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        let Some((additions, deletions, rest)) = parse_counts(record) else {
            index += 1;
            continue;
        };

        let mut path = rest.to_string();
        let mut previous_path = None;
        if path.is_empty() {
            previous_path = records.get(index + 1).map(|path| (*path).to_string());
            path = records.get(index + 2).copied().unwrap_or("").to_string();
            index += 2;
        }
        index += 1;
        if path.is_empty() {
            continue;
        }

        files.push(TurnDiffFileSummary {
            path,
            previous_path,
            additions,
            deletions,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}

fn parse_counts(record: &str) -> Option<(u32, u32, &str)> {
    let mut fields = record.splitn(3, '\t');
    let additions = fields.next()?;
    let deletions = fields.next()?;
    let rest = fields.next()?;
    if !is_count(additions) || !is_count(deletions) {
        return None;
    }
    Some((
        additions.parse().unwrap_or(0),
        deletions.parse().unwrap_or(0),
        rest,
    ))
}

fn is_count(value: &str) -> bool {
    value == "-" || (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
}

/// A file is an import when the turn did not author either of its paths.
///
/// `None` means HEAD never moved between the checkpoints, so no path is known
/// to be imported and all of them belong to the turn.
pub fn is_git_import(file: &TurnDiffFileSummary, authored_paths: Option<&HashSet<String>>) -> bool {
    let Some(authored_paths) = authored_paths else {
        return false;
    };
    !authored_paths.contains(&file.path)
        && file
            .previous_path
            .as_ref()
            .is_none_or(|path| !authored_paths.contains(path))
}

#[cfg(test)]
mod tests {
    // Ported from apps/server/src/checkpointing/Diffs.test.ts (T3 Code v0.1.11, MIT).
    use super::*;

    #[test]
    fn sorts_files_and_preserves_addition_and_deletion_counts() {
        let numstat = ["0\t2\tsrc/b.ts", "2\t1\ta.txt", ""].join("\0");
        assert_eq!(
            parse_turn_diff_files_from_numstat(&numstat),
            vec![
                TurnDiffFileSummary {
                    path: "a.txt".into(),
                    previous_path: None,
                    additions: 2,
                    deletions: 1,
                },
                TurnDiffFileSummary {
                    path: "src/b.ts".into(),
                    previous_path: None,
                    additions: 0,
                    deletions: 2,
                },
            ]
        );
    }

    #[test]
    fn preserves_both_paths_for_renames_and_copies() {
        let numstat = [
            "0\t0\t",
            "src/old.ts",
            "src/new.ts",
            "2\t1\t",
            "src/source.ts",
            "src/copied.ts",
            "1\t0\tother.ts",
            "",
        ]
        .join("\0");
        assert_eq!(
            parse_turn_diff_files_from_numstat(&numstat),
            vec![
                TurnDiffFileSummary {
                    path: "other.ts".into(),
                    previous_path: None,
                    additions: 1,
                    deletions: 0,
                },
                TurnDiffFileSummary {
                    path: "src/copied.ts".into(),
                    previous_path: Some("src/source.ts".into()),
                    additions: 2,
                    deletions: 1,
                },
                TurnDiffFileSummary {
                    path: "src/new.ts".into(),
                    previous_path: Some("src/old.ts".into()),
                    additions: 0,
                    deletions: 0,
                },
            ]
        );
    }

    #[test]
    fn keeps_binary_files_and_empty_files_with_zero_line_changes() {
        let numstat = ["-\t-\timage.png", "0\t0\tempty.txt", ""].join("\0");
        assert_eq!(
            parse_turn_diff_files_from_numstat(&numstat),
            vec![
                TurnDiffFileSummary {
                    path: "empty.txt".into(),
                    previous_path: None,
                    additions: 0,
                    deletions: 0,
                },
                TurnDiffFileSummary {
                    path: "image.png".into(),
                    previous_path: None,
                    additions: 0,
                    deletions: 0,
                },
            ]
        );
    }

    #[test]
    fn preserves_unicode_tabs_line_endings_and_spaces_in_paths() {
        let path = " café\tline\r\nname.txt ";
        let numstat = format!("3\t2\t\0old\tname\n.txt\0{path}\0");
        assert_eq!(
            parse_turn_diff_files_from_numstat(&numstat),
            vec![TurnDiffFileSummary {
                path: path.into(),
                previous_path: Some("old\tname\n.txt".into()),
                additions: 3,
                deletions: 2,
            }]
        );
        assert_eq!(
            parse_turn_diff_files_from_numstat(&format!("1\t0\t{path}\0")),
            vec![TurnDiffFileSummary {
                path: path.into(),
                previous_path: None,
                additions: 1,
                deletions: 0,
            }]
        );
    }

    #[test]
    fn git_imports_are_files_the_turn_did_not_author() {
        let authored: HashSet<String> = ["a.txt".to_string()].into_iter().collect();
        let own = TurnDiffFileSummary {
            path: "a.txt".into(),
            previous_path: None,
            additions: 1,
            deletions: 0,
        };
        let imported = TurnDiffFileSummary {
            path: "upstream.txt".into(),
            previous_path: None,
            additions: 1,
            deletions: 0,
        };
        assert!(!is_git_import(&own, Some(&authored)));
        assert!(is_git_import(&imported, Some(&authored)));
        assert!(!is_git_import(&imported, None));
    }
}
