//! A headless integration seam: drive production Project state and real remote
//! protocol clients; the fixture supplies ticks and authored task/result files.
#[allow(dead_code)]
#[path = "../../../atrium-app/src/project.rs"]
mod project;

use super::*;
use project::{
    Project, Role, TaskStatus, Workstream, controller_notice, wake_allowed, with_notices,
};
use std::{fs, path::Path};

struct CodexSession {
    client: CodexClient,
    events: futures::channel::mpsc::UnboundedReceiver<Incoming>,
    thread: String,
}
impl CodexSession {
    async fn start(node: &Node) -> Self {
        let (attachment, keep) = node.spawn("codex");
        let (client, events) = CodexClient::remote(attachment.reader, attachment.writer, keep)
            .await
            .unwrap();
        assert!(client.user_agent.starts_with("mock-codex"));
        let thread = client
            .thread_start(StartThread {
                cwd: Some(node.root.to_string_lossy().into_owned()),
                ..Default::default()
            })
            .await
            .unwrap()
            .id;
        Self {
            client,
            events,
            thread,
        }
    }

    async fn turn(&mut self, text: &str) -> String {
        let turn = self
            .client
            .turn_start(
                &self.thread,
                TurnRequest {
                    text: text.into(),
                    mode: Some(AccessMode::Auto),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut received = String::new();
        loop {
            match self
                .events
                .next()
                .await
                .expect("mock closed before turn completion")
            {
                Incoming::Notification { method, params } => {
                    if params["threadId"].as_str() != Some(&self.thread) {
                        continue;
                    }
                    if method == "item/agentMessage/delta"
                        && let Some(delta) = params["delta"].as_str()
                    {
                        received.push_str(delta);
                    }
                    if method == "turn/completed" && params["turn"]["id"].as_str() == Some(&turn) {
                        assert_eq!(params["turn"]["status"], "completed");
                        return received;
                    }
                }
                Incoming::Request { id, method, params } => {
                    if matches!(
                        translate::request(&id, &method, &params),
                        Some(AgentEvent::ApprovalRequested(_))
                    ) {
                        self.client
                            .answer_approval(id, ApprovalDecision::Accept)
                            .unwrap();
                    } else {
                        self.client.reject_request(id, &method).unwrap();
                    }
                }
                Incoming::Closed => panic!("mock closed before turn completed"),
            }
        }
    }
}

enum Session {
    Codex(CodexSession),
    Acp {
        client: AcpClient,
        events: futures::channel::mpsc::UnboundedReceiver<Incoming>,
        thread: String,
    },
}
impl Session {
    async fn start(node: &Node, backend: &str) -> Self {
        if backend == "codex" {
            return Self::Codex(CodexSession::start(node).await);
        }
        let (attachment, keep) = node.spawn(backend);
        let (client, events) = AcpClient::over(
            backend.into(),
            attachment.reader,
            attachment.writer,
            Some(keep),
        )
        .await
        .unwrap();
        assert!(client.info.name.contains("mock"));
        let thread = client.new_session(&node.root).await.unwrap().session_id;
        Self::Acp {
            client,
            events,
            thread,
        }
    }
    fn thread(&self) -> &str {
        match self {
            Self::Codex(session) => &session.thread,
            Self::Acp { thread, .. } => thread,
        }
    }
    async fn turn(&mut self, text: &str) -> String {
        let Self::Acp {
            client,
            events,
            thread,
        } = self
        else {
            let Self::Codex(session) = self else {
                unreachable!()
            };
            return session.turn(text).await;
        };
        let mut transcript = Transcript::new();
        for event in client.begin_turn(thread).1 {
            transcript.apply(&event);
        }
        let prompt = client.prompt(thread, text, &[], &[]);
        tokio::pin!(prompt);
        loop {
            tokio::select! {
                ended = &mut prompt => {
                    while let Ok(incoming) = events.try_recv() {
                        for event in client.translate(incoming) { transcript.apply(&event); }
                    }
                    assert!(ended.iter().any(|event| matches!(event, AgentEvent::TurnCompleted { status: TurnStatus::Completed, .. })));
                    return transcript.entries().iter().filter_map(|entry| match &entry.kind {
                        EntryKind::Item(Item::Agent { text, .. }) => Some(text.as_str()),
                        _ => None,
                    }).collect();
                }
                incoming = events.next() => {
                    for event in client.translate(incoming.expect("ACP mock closed early")) {
                        if let AgentEvent::ApprovalRequested(request) = &event {
                            client.answer_permission(request.request_id.clone(), ApprovalDecision::Accept).unwrap();
                        }
                        transcript.apply(&event);
                    }
                }
            }
        }
    }
}

fn complete(project: &mut Project, key: &str, result: &str) {
    let task = project.task(key).expect("claimed task");
    let path = project.root.join(task.rel_path());
    let previous = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{previous}\n## Result\n{result}\n")).unwrap();
    project.set_task_status(key, TaskStatus::Done).unwrap();
}

async fn start_child(
    project: &mut Project,
    node: &Node,
    key: &str,
    role: Role,
    backend: &str,
) -> (String, Session) {
    let task = project
        .startable()
        .into_iter()
        .find(|task| task.key() == key)
        .unwrap();
    let stream = project.stream_from_task(task, backend).unwrap();
    assert_eq!(stream.role, role);
    let id = stream.id.clone();
    project.set_task_status(key, TaskStatus::Running).unwrap();
    assert!(project.startable().iter().all(|task| task.key() != key));
    project.file.streams.push(stream.clone());
    project.save().unwrap();
    let mut session = Session::start(node, backend).await;
    project.bind_thread(&id, session.thread()).unwrap();
    let kickoff = project.kickoff(&stream);
    assert!(!session.turn(&kickoff).await.is_empty());
    (id, session)
}

async fn deliver(project_root: &Path, parent: &str, source: &str, session: &mut Session) {
    let mut project = Project::open(project_root).unwrap();
    assert_eq!(project.pending_report_parents(), [parent]);
    let unread = project.unread(parent);
    assert_eq!(unread.len(), 1);
    assert!(unread[0].text.contains(source));
    if project
        .controller()
        .is_some_and(|controller| controller.id == parent)
    {
        assert!(controller_notice(unread[0], parent));
    }
    let now = Instant::now();
    assert!(!wake_allowed(None, now, true, false, false));
    assert!(wake_allowed(None, now, false, false, false));
    assert!(!wake_allowed(Some(now), now, false, false, false));
    let prompt = with_notices(&unread, "Review this result.");
    assert!(prompt.contains(source));
    let answer = session.turn(&prompt).await;
    assert!(answer.contains("[Atrium] New notices for you:") && answer.contains(source));
    project.mark_read(parent);
    project.save().unwrap();
    assert!(
        Project::open(project_root)
            .unwrap()
            .pending_report_parents()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built mock binaries; headless, no accounts or GPUI"]
async fn native_controller_lead_worker_completion_wakes_both_parents() {
    for lead_backend in ["codex", "claude"] {
        let node = Node::start_with_hierarchy(true);
        tokio::time::timeout(Duration::from_secs(60), async {
        let mut project = Project::open(&node.root).unwrap();
        let mut controller = Session::start(&node, "codex").await;
        project.file.streams.push(Workstream {
            id: "controller".into(),
            role: Role::Controller,
            name: "Controller".into(),
            backend: "codex".into(),
            goal: "Coordinate validation".into(),
            ..Default::default()
        });
        project.bind_thread("controller", controller.thread()).unwrap();
        let kickoff = project.kickoff(project.controller().unwrap());
        assert!(controller.turn(&kickoff).await.contains("mock-codex"));
        project.file.approve_leads = true;
        let lead_task = project.add_task("controller", "Web", "# Web\nstatus: open\n\nDeliver web.\n\n## Roadmap\n\n### Phase 1\n- [ ] Validate").unwrap();
        assert!(project.startable().is_empty());
        project.file.approved.push(lead_task.clone());
        project.save().unwrap();
        let (lead, mut lead_session) = start_child(&mut project, &node, &lead_task, Role::Lead, lead_backend).await;
        let worker_task = project.add_task(&lead, "Fixture", "# Fixture\nstatus: open\n\nValidate fixture.").unwrap();
        let (worker, _worker_session) = start_child(&mut project, &node, &worker_task, Role::Worker, "codex").await;
        complete(&mut project, &worker_task, "Worker fixture verified");
        assert_eq!(project.report_finished(&worker).unwrap(), Some(lead.clone()));
        assert_eq!(project.report_finished(&worker).unwrap(), None);
        deliver(&node.root, &lead, "Worker fixture verified", &mut lead_session).await;
        let mut project = Project::open(&node.root).unwrap();
        complete(&mut project, &lead_task, "Lead fixture verified");
        assert_eq!(project.report_finished(&lead).unwrap().as_deref(), Some("controller"));
        deliver(&node.root, "controller", "Lead fixture verified", &mut controller).await;
        assert_eq!(Project::open(&node.root).unwrap().notices.len(), 2);
    }).await.expect("native mock hierarchy exceeded 60 seconds");
    }
}
