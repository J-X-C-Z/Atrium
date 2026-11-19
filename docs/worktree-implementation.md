# Building worktrees for workstreams

A brief for giving each lead and pool worker its own git worktree, as Claude
Code's desktop app does per session. Today every workstream shares one
checkout, so parallel agents overwrite each other's files and a half-done
change of one breaks another's build. Read `docs/hierarchy-implementation.md`
and `docs/autopilot-implementation.md` first.

## The model (done)

- `ProjectFile.worktrees: bool`: off by default; when on, the scheduler
  below applies. Add a toggle next to the autopilot in the project menu
  ("Give each agent its own worktree"): `Workspace::set_worktrees(on)`
  saving the flag. That toggle is the only UI change needed.
- `Project::work_dir(id)`: where the session runs. It is
  `.atrium/worktrees/<id>` for leads and workers when on, and the root for the
  controller and when off.
- `Project::branch_of(id)`: `atrium/<id>`.

## What to build

All git calls go through one small module (`crates/atrium-app/src/git.rs`)
that shells out to `git` with `-C <dir>` and returns `anyhow::Result`. Keep
the decisions (which branch to base on, what to merge where) in plain
functions and test them against a temporary repository (`git init` in the
test).

### 1. Creating

Before starting a session for stream `s` in a project with worktrees on
(in `start_stream`):

1. Not a git repository → leave `worktrees` off for this project, post one
   notice to `you`, and run in the root.
2. Add `.atrium/` to `.git/info/exclude` once, so worktrees and the atrium
   never get committed.
3. Base branch:
   - a lead uses the current branch of the root;
   - a pool worker uses its lead's branch, `atrium/<lead>`.
4. `git worktree add -b atrium/<id> .atrium/worktrees/<id> <base>`. If the
   branch already exists (a restart), reuse it with `git worktree add <dir>
   atrium/<id>`.
5. Symlink `<worktree>/.atrium` → `<root>/.atrium`. Every agent then sees the
   same task files, notices and plan through the paths its kickoff names.
6. Copy the files listed in `<root>/.worktreeinclude` (gitignored files such
   as `.env`; one glob per line) into the worktree.
7. Start the session with `cwd = project.work_dir(id)`. MCP servers keep
   `--project <root>`.

Scope checks (`Project::crossing`) receive paths relative to the worktree:
strip `work_dir(id)` instead of the root.

### 2. A pool worker's task flows into its lead's branch

When a pool worker finishes a task (its task file turns `done`):

1. In its worktree, run `git add -A`, then `git commit -m "<task title>
   (<task key>)"`, skipping the commit when nothing changed.
2. Merge into the lead's branch from the lead's worktree with `git merge
   --no-ff atrium/<worker>`. Only do this when the lead's worktree is clean;
   otherwise wait for the next tick.
3. On a conflict: `git merge --abort`, post the lead a notice naming the task
   and the conflicting files, and leave the task's result as it is. The lead
   resolves the conflict, or asks a worker to.
4. Before the worker claims its next task, rebase its branch onto the lead's
   branch with `git rebase atrium/<lead>`, so it starts from everything merged
   so far.

### 3. A lead's work reaches the main branch

Never automatically. When a lead's task is `done`, its branch is ready.
- Add `Workspace::merge_lead(id)`: it merges `atrium/<id>` into the root's
  current branch, with `--no-ff`, only when the root is clean, and reports
  conflicts as a notice.
- The UI will call it from a "Merge into <branch>" button on the lead's card
  (I add the button once the function exists).

### 4. Cleaning up

- A retired pool worker's worktree: once its branch is merged into the
  lead's branch, `git worktree remove` it and delete the branch.
- A removed workstream: if its worktree has uncommitted changes or unmerged
  commits, keep it and post a notice to `you`. Otherwise remove it.
- Never delete a lead's branch automatically.

## Tests

1. Creating twice reuses the branch; `.atrium` is excluded and symlinked.
2. A worker's commit merges into the lead's branch; a conflict aborts
   cleanly and produces one notice.
3. A worker rebases onto the lead before its next task.
4. Cleanup keeps a worktree with unmerged commits.
5. A non-git folder falls back to the root with one notice.

## Done when

- Tests and clippy are clean.
- On Xvfb with the mocks, a lead with two pool workers works in three
  worktrees. Make the mock edit a file per task (mock-codex already emits a
  file change when asked to "edit"). The lead's branch ends with both tasks
  merged, and the root checkout is untouched until "merge" runs.
