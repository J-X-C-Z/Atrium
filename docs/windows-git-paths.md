# Windows: git worktree and checkpoint paths

`Native backend acceptance` fails on `windows-2025` and passes on macOS and
Ubuntu. It failed the same way when #2 was merged, and #3 added one more
test that fails for the same reason. Every failure is
`os error 123` ("The filename, directory name, or volume label syntax is
incorrect"), so a path Atrium builds is not a valid Windows path.

## Failing tests (run 44983696855)

| Test | Panics at |
| --- | --- |
| `git::tests::creation_reuses_checkout_and_branch_and_preserves_dirty_root` | `git.rs:860` |
| `git::tests::worker_merge_rebase_and_safe_retirement` | `git.rs:947` |
| `git::scheduler_tests::pool_claim_waits_for_merge_and_rebase_then_lead_is_merged_only_on_request` | `git.rs:1109` (`prepare_stream_tree`) |
| `git::scheduler_tests::restarted_worker_conflict_posts_once_and_preserves_task_and_branches` | `git.rs:1145` (`prepare_stream_tree`) |
| `git::scheduler_tests::non_git_falls_back_with_one_notice_and_reused_worker_branch_survives_retirement` | `git.rs:1194` |
| `repo::tests::a_move_bundle_carries_uncommitted_files_into_a_worktree` | `repo.rs:793` |
| `repo::tests::a_file_or_one_hunk_can_be_undone` | `repo.rs:827` |
| `repo::tests::checkpoints_restore_and_undo` | `repo.rs:868` |
| `repo::tests::a_chat_worktree_merges_back_and_goes` | `repo.rs:904` |

## Where to look

Everything that fails creates a worktree or works with checkpoints and
hunks. The usual causes of error 123 on Windows are:

- a path that git prints (`git worktree list`, `rev-parse --show-toplevel`,
  `--git-common-dir`) taken as is. These come back as `C:/...` and can be
  joined onto another absolute path, giving `C:\...\C:/...`;
- a ref or thread id used in a file name. `:`, `*`, `?`, `"`, `<`, `>` and
  `|` are not allowed in Windows file names, and the tests use ids such as
  `thread/1`;
- `canonicalize()` returning a `\\?\C:\...` path that is then compared with,
  or joined onto, an ordinary one (`dunce::canonicalize` avoids this);
- the `.atrium` link from a worktree to the root. On Windows this should be
  a junction (or a copy of the folder), not a symlink.

## Done when

- `cargo test -p atrium-project` passes on `windows-2025` in CI, without
  `#[cfg(not(windows))]` on the tests above;
- the whole `Native backend acceptance` workflow is green on all three
  systems;
- the fix keeps macOS and Linux paths exactly as they are now.
