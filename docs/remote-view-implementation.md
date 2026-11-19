# Building the phone view

A brief for following a project from a phone. Claude's desktop app does this
with Remote Control; Atrium does it over the tailnet. The use is the
unattended project running all night: you check progress, answer an
approval, or post a notice from bed.

## Shape

Atrium (the desktop app) serves a small HTTP API and one web page on the
machine's tailnet address. Your phone, on the same tailnet, opens
`http://<machine>:7879/`. Nothing goes through a cloud service.

The **API and the server are yours** (backend). The **web page is mine**
(frontend): I build it against the API below once it exists. Until then,
serve a placeholder `index.html` that lists the projects from
`/api/projects`.

## Server

- Off by default. Add a setting "Let my other devices view projects"
  (`Settings.remote_view: bool`); I add the switch to the settings dialog.
- Bind to the tailnet addresses only (as `atrium-node` does; reuse its
  `tailscale.rs` approach) on port 7879, plus `127.0.0.1` for testing.
- Authorise like `atrium-node`: only peers that `tailscale whois` reports
  under the owner's login. See `docs/node-protocol.md`, "Who may connect".
  Without Tailscale, serve on localhost only.
- Run it on a background thread with a small blocking HTTP server (the
  workspace already depends on `tungstenite`; add `tiny_http` or similar).
  Hand requests to the GPUI side through a channel, and answer from
  snapshots the workspace publishes. Never block the UI thread.

## API (JSON)

| Method and path | Returns or does |
| --- | --- |
| `GET /api/projects` | `[{root, name, autopilot, streams, working, needs_you}]` |
| `GET /api/projects/:ix` | A snapshot of one project. `streams` is each with `id, name, role, backend, state, parent, task` (the parsed task: title, status, phases with steps, result), `pool` (`Project::pool` for leads), `needs_you` (pending approval or question, with its text). Also `notices` (last 50) and `plan`. |
| `GET /api/threads/:id?after=<n>` | The transcript rows after index `n`, as plain text items: role, text, tool title, status. Enough to read along, not the full fidelity. |
| `POST /api/approvals` `{thread, request_id, decision}` | Answers an approval, as the desktop's buttons do. `decision`: `accept`, `accept_for_session`, `decline`. |
| `POST /api/questions` `{thread, request_id, answers}` | Answers a question. |
| `POST /api/projects/:ix/notices` `{to, text}` | Posts a notice from `you`. |
| `POST /api/threads/:id/messages` `{text}` | Sends a message to a workstream, the same as typing it. |
| `GET /api/events` | Server-sent events: `{"type":"changed","project":ix}` whenever a snapshot changes, and `{"type":"needs_you",...}`, so the page refreshes without polling. |

`:ix` is the project's index in `Workspace::projects`. Snapshots are built
from the same data the overview uses (`stream_views`, `Project::pool`,
`Task`), so nothing new is computed.

### Desktop backend contract

The server follows the live `remote_view` setting. To enable it before the
settings switch ships, set `"remote_view": true` in the desktop
`settings.json` and restart Atrium. On macOS this file is in
`~/Library/Application Support/Atrium`, on Windows in `%APPDATA%/Atrium`.
An omitted or false setting leaves the server off.

Snapshots refresh about once per second. The project list counts streams
in the working and needs-you states. Each detail stream supplies its
`thread_id` for transcript and action URLs, and a `needs_you` array with
the original numeric or string `request_id`. Stream states are `starting`,
`offline`, `working`, `needs_you`, `failed` and `idle`. Pool fields
`ready`, `waiting`, `running` and `workers` are arrays, `done`, `failed`
and `total` are counts. Transcript rows have a zero-based `index`, `role`,
`text`, `tool_title` and `status`. Omitting `after` reads all rows.
A changed event can also update a previously streamed row.

Send POSTs as `application/json`. For a question, `answers` is an object
such as `{"color":["Blue"]}` with one selection or free-text answer per
question. Notice recipients are an array of stream IDs or `["all"]`.
Approvals, questions and persisted notices return 204. Submitting a message
returns 202, with the eventual backend result in the transcript. A stale
request returns 409. Only threads bound to workstreams are addressable.

The browser uses the page's own HTTP origin and port. A native shell can
use its native HTTP client. Loopback hosts, the bound tailnet IP, and the
desktop's own MagicDNS name (full or short) are checked against the socket.
SSE messages are ordinary `data:` JSON events. On reconnect the server
announces current projects and outstanding requests. It renews the
connection every minute to recheck authorization.

Pushes require remote view and a nonempty `push_topic`, either a full
ntfy topic URL such as `https://notify.example/atrium-private` or a topic
name on ntfy.sh. The notification title identifies the event, the body
contains the project and stream names, and the Click header contains the
`atrium://project/<ix>/stream/<id>` link. The second distinct failed turn
alerts once until a successful turn resets the streak. A lead with an
assigned task alerts on completion when its task status is done.

## The app

The phone side ships as an app, not just a web page. The screens are
designed in the canvas "Atrium 手机 App": connect, projects, project
progress, lead, needs-you inbox, conversation and notices.

- **One UI, two shells.** The UI side builds the screens as a web app
  against the API above. It is served at `/` for any browser and wrapped as
  a native app with **Tauri 2** (iOS and Android; Rust, like the rest of
  Atrium). The shell adds what a page cannot do:
  - find computers on the tailnet (`/api/projects` on each peer that
    `tailscale status --json` lists, port 7879);
  - remember the chosen computer;
  - background refresh;
  - notifications.
- **Notifications.** A phone app in the background cannot hold the event
  stream open. When something needs the user (an approval, a question, a
  lead finished, a failure loop), the server also sends a push. Default:
  [ntfy](https://ntfy.sh)-compatible, to a topic the user sets (self-hosted
  ntfy keeps it off third-party servers). The payload is a title and the
  project and stream names only, never code or commands. Tapping it opens
  the matching screen through `atrium://project/<ix>/stream/<id>`. Add
  `Settings.push_topic` and send pushes from the same place as the desktop
  notifications (`alerts.rs`).
- **Answering from the lock screen** (later): approvals as actionable
  notifications that POST `/api/approvals` through the app.

## Tests

1. The snapshot of the demo fixture (`.atrium` with a controller, leads and a
   pool) matches the overview's numbers: phases, pool counts, needs-you.
2. An approval answered through the API resolves it in the desktop UI.
3. A request from an address `whois` does not vouch for gets 403. Mock
   `whois` the way the node tests do.
4. With the setting off, nothing listens.

## Done when

- Tests and clippy are clean.
- With the mocks on Xvfb, `curl` against 127.0.0.1:7879 can follow a
  project, answer an approval, and post a notice, and the desktop updates.
