# Atrium memory: the contract for a memory service

Atrium keeps one memory that every agent shares: Codex, Claude Code and the
other ACP agents, on any project. A **memory service** is a separate
program that learns from finished conversations and keeps the memory tidy.
A Rust Hermes is one example: it reads conversations, saves what is worth
keeping, and turns workflows that worked into skills. Atrium does not
summarize anything itself.

The contract is a folder on disk and the `atrium-memory` crate. A service
in Rust should depend on the crate (`crates/atrium-memory`, a git
dependency), so its writes go through the same checks and journal as
Atrium's. A service in another language follows the formats below.

## Who does what

| | Atrium | Memory service | Hindsight (optional) |
|---|---|---|---|
| Exports conversations | ✓ `sessions/` | reads them | gets each one through `retain` |
| Says which conversations are finished | ✓ `inbox/` | takes requests and deletes them | |
| Decides what is worth keeping | | ✓ | finds facts and experiences itself |
| Writes entries and skills | the user, from the memory page; agents, through the `remember` tool | ✓ through `Store::apply` | |
| Nightly tidying (merges duplicates, archives stale entries) | sets `nightly_tidy` | ✓ when `nightly_tidy` is on | |
| Puts memory into new sessions | ✓ entries in use, the skill index, and what Hindsight recalls | | `recall` |

## The folder

The default is `<config dir>/Atrium/memory`, which is
`~/Library/Application Support/Atrium/memory` on macOS. `ATRIUM_MEMORY_DIR`
overrides it.

```text
config.json                Config (below). Atrium writes it; services read it.
global/entries.json        entries for every project: about the user, and general
projects/<key>/entries.json  entries for one project
projects/<key>/path        the project's folder
projects/<key>/skills/     skills for that project only
skills/<name>/SKILL.md     skills for every project
sessions/<thread>.jsonl    conversations, as Atrium exports them
inbox/<ms>-<thread>.json   conversations waiting for review
journal.jsonl              every change to the entries
.lock                      held while a batch is written
```

`<key>` is `atrium_memory::project_key(project_root(cwd))`. It is the git
root's folder name followed by 8 hex digits of its path, such as
`atrium-3f2a9c1d`.

## config.json

```json
{
  "inject": true,               // put memory into new sessions
  "budget_tokens": 2000,        // the most tokens that go into one session
  "learn": true,                // export conversations and queue them
  "confirm_learned": false,     // what a service adds waits for the user
  "nightly_tidy": false,        // the service may tidy once a night
  "review_after_idle_min": 10,  // a conversation is queued after this much quiet
  "off_agents": [],             // backend ids that get no memory
  "hindsight": { "enabled": true, "url": "http://localhost:8888", "api_key": null,
                 "bank": "atrium", "project_banks": true, "recall_tokens": 800, "retain": true }
}
```

A service follows `confirm_learned` by passing `config.landing()` to
`Store::apply`. It tidies only while `nightly_tidy` is true. The Hindsight
section tells a service where Hindsight is, so the service can use it as
its long-term store. Atrium has already retained each conversation there,
in bank `atrium-<key>` (the project's) or `atrium` (global).

## Conversations: sessions/<thread>.jsonl

The first line describes the conversation:

```json
{"type":"session","version":1,"thread":"…","agent":"claude","cwd":"/code/app",
 "project":"app-3f2a9c1d","title":"Fix the build","started_ms":…,"updated_ms":…}
```

Each further line is `{"type":"user"|"agent"|"tool","turn":N,"text":"…"}`:
- `user` lines are what the user wrote, with Atrium's memory block removed.
- `tool` lines are short: a command with its exit code and the last lines
  of its output, the files edited, a tool's name, or an error.
- Reasoning and approvals are left out.

Atrium rewrites the whole file whenever the conversation grows.
`Session::transcript()` gives it as plain text.

## The inbox: inbox/<ms>-<thread>.json

```json
{"version":1,"thread":"…","agent":"codex","cwd":"…","project":"app-3f2a9c1d",
 "session":"…/sessions/….jsonl","turns":4,"reason":"idle","at_ms":…}
```

`reason` is one of:
- `idle`: the conversation has been quiet for `review_after_idle_min`;
- `asked`: the user chose "Learn from this conversation".

Each conversation has at most one request waiting. A newer request
replaces the older one. To process the queue:
1. List requests with `Store::pending()`, oldest first.
2. Read each request's session.
3. Delete the request file once it is handled.

A conversation that continues later is queued again, with all of its turns.

## Entries: entries.json

```json
[{"id":"3f2a9c1d0b4e","text":"Run the tests with cargo test --workspace",
  "target":"notes","state":"active","pinned":false,"author":"hermes",
  "created_ms":…,"updated_ms":…}]
```

- `target`:
  - `user` is about the user. Keep these global.
  - `notes` is everything else.
- `state`:
  - `active` is what agents see.
  - `pending` waits for the user. Agents don't see it, and it does not count toward the limit.
  - `archived` is kept out of sight. It can be put back in use.
- Limits apply to `active` entries only. Each scope may hold 1,400
  characters of `user` entries and 2,400 of `notes` entries. A batch that
  goes over fails, and nothing is written. To fit, archive, remove or
  shorten entries in the same batch.
- `author` is the writer:
  - `user` for the user;
  - `agent:<backend>` for an agent's `remember`;
  - the service's own name for a service.
- Entries written by the user, and pinned entries, belong to the user. A
  service's batch that patches, removes or archives one fails with
  `OpError::UsersEntry`.

### Changing entries

Always change entries in one batch through `Store::apply(scope, ops, author, landing)`:

```rust
let store = Store::new(root);
let config = Config::load(store.root());
store.apply(
    &Scope::Project(key),
    &[
        Op::Archive { old: "cargo test".into() },          // an id, or a unique piece of text
        Op::Add { content: "Tests: cargo nextest run".into(), target: Target::Notes },
    ],
    "hermes",
    config.landing(),
)?;
```

The ops are `Add`, `Patch { old, new }`, `Remove`, `Archive`, `Restore`
and `Pin` (`Pin` is for the user only). A batch also does the following:
- Exact duplicates change nothing.
- Text that tries to steer agents, text with hidden characters, and text
  holding secrets is refused (`scan::check`).
- Each batch is one line of `journal.jsonl`. The user can undo any batch
  that nothing has changed since, from the memory page.

A service in another language must take `.lock` (create it exclusively;
it may take over a lock older than 10 s). It must rewrite `entries.json`
through a temporary file and rename, and append one `Change` line to
`journal.jsonl`.

### What is worth keeping

Keep an entry to one or two sentences, and only what will matter again:
- the user's stated preferences;
- a project's commands and conventions;
- a fix that took real effort, and why;
- where things are.

Leave out the following:
- what the code or the docs already say;
- one-off details;
- anything secret.

The whole memory goes into every new session, so prefer merging entries
to adding new ones.

## Skills

A skill is a folder holding a `SKILL.md` that starts with front matter:

```markdown
---
name: release
description: Cut a release: bump versions, tag, build the packages, write the notes
---

# Release
When to use … Steps … Pitfalls … How to check it worked …
```

- **Where they go**:
  - global skills go in `skills/<name>/`;
  - a project's own skills go in `projects/<key>/skills/<name>/`, and win
    over a global skill with the same name.
- **What agents see**: the index only, one line per skill with its name,
  description and path. They read the file when a skill applies.
- **When to write one**: after a conversation that took many steps to do
  something that will come up again, write or update a skill rather than an
  entry.

## What agents get

**At the start of a new conversation** (when `inject` is on, the agent
isn't listed in `off_agents`, and it runs on this computer):
- For Codex, a block goes into the thread's `developerInstructions`.
- For ACP agents (Claude Code and others), the same block goes in front of
  the first message, inside `<atrium-memory>…</atrium-memory>`. Atrium
  shows the message without it.

The block is built in this order, until `budget_tokens` is used:
1. about the user;
2. this project;
3. general notes;
4. what Hindsight recalled for the first message;
5. the skill index.

It is made once per conversation, so later turns reuse the prompt cache.

**Throughout the conversation**, the MCP server `atrium-memory` (`atrium
memory-mcp --root … --cwd … --agent …`) offers two tools:
- **`recall(query)`** searches entries, past conversations with any agent,
  and Hindsight when it is on.
- **`remember(text, about: user|project|general)`** adds an entry by
  `agent:<backend>`. It respects `confirm_learned`, and it is retained in
  Hindsight when that is on.

There are only two tools, so the tool list each request carries stays
small. Atrium does not attach Hindsight's own MCP endpoint, which has 27
tools.

## Checks

`cargo test -p atrium-memory` covers:
- batches and limits, pending entries, entries that belong to the user,
  and undo;
- the safety scan;
- skills and how a project's skill shadows a global one;
- session export and search, and the inbox;
- how the block is built within its budget;
- the Hindsight client against a stand-in server;
- the MCP tools, with one agent remembering and another recalling.
