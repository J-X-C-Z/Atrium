use crate::ProjectSnapshot;
use atrium_core::{ApprovalKind, EntryKind, Item, ItemStatus, Transcript};
use atrium_project::{Project, Role, Workstream};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Default)]
pub struct Chat {
    pub rows: Arc<Vec<Value>>,
    pending: Vec<Value>,
}

fn status(s: ItemStatus) -> &'static str {
    match s {
        ItemStatus::InProgress => "working",
        ItemStatus::Completed => "completed",
        ItemStatus::Failed => "failed",
        ItemStatus::Declined => "declined",
    }
}

impl Chat {
    pub fn from_transcript(transcript: &Transcript) -> Self {
        let mut pending = vec![];
        let rows = transcript.entries().iter().enumerate().map(|(index, entry)| {
            let default_status = if entry.live { "working" } else { "completed" };
            let mut text = String::new();
            let mut title = None;
            let mut state = default_status;
            let role = match &entry.kind {
                EntryKind::Item(Item::Agent { text: s, .. }) => {text.clone_from(s); "agent"}
                EntryKind::Item(Item::User { text: s, .. }) => {text.clone_from(s); "user"}
                EntryKind::Item(Item::Reasoning { text: s, .. }) => {text.clone_from(s); "reasoning"}
                EntryKind::Item(item) => {
                    let (label, item_status) = match item {
                        Item::Command { command, status,.. } => (command.as_str(),Some(*status)),
                        Item::FileChange { status,.. } => ("File change",Some(*status)),
                        Item::Tool { name,status,.. } => (name.as_str(),Some(*status)),
                        Item::WebSearch { query,.. } => (query.as_str(),None),
                        Item::Image { status,.. } => ("Image",Some(*status)),
                        Item::SubAgent { label,status,.. } => (label.as_str(),Some(*status)),
                        Item::Other { label,.. } => (label.as_str(),None),
                        _ => unreachable!(),
                    };
                    title = Some(label);
                    state = item_status.map_or(default_status, status);
                    "tool"
                }
                EntryKind::Approval { request, answer, resolved } => {
                    text = match &request.kind {
                        ApprovalKind::Command {command,cwd,reason} => [reason.as_deref().unwrap_or(""),command,cwd].join("\n").trim().to_owned(),
                        ApprovalKind::FileChange {reason,grant_root} => [reason.as_deref().unwrap_or("File change"),grant_root.as_deref().unwrap_or("")].join("\n").trim().to_owned(),
                        ApprovalKind::Tool {title,detail} => [title,detail.as_deref().unwrap_or("")].join("\n").trim().to_owned(),
                    };
                    if answer.is_none() && !resolved {
                        state = "pending";
                        pending.push(json!({"kind":"approval","request_id":request.request_id,"text":text}));
                    } else { state = "resolved"; }
                    "approval"
                }
                EntryKind::Question {request,answers,resolved} => {
                    text = request.questions.iter().map(|q| q.question.as_str()).collect::<Vec<_>>().join("\n");
                    if answers.is_none() && !resolved {
                        state = "pending";
                        let questions: Vec<_> = request.questions.iter().map(|q| json!({"id":q.id,"header":q.header,"question":q.question,
                            "options":q.options,"allow_other":q.allow_other,"secret":q.secret})).collect();
                        pending.push(json!({"kind":"question","request_id":request.request_id,"text":text,"questions":questions}));
                    } else { state = "resolved"; }
                    "question"
                }
                EntryKind::Plan {explanation,steps} => {
                    text = explanation.iter().map(String::as_str).chain(steps.iter().map(|s|s.step.as_str())).collect::<Vec<_>>().join("\n");
                    "plan"
                }
                EntryKind::Error(s) => { text.clone_from(s); state = "failed"; "error" }
            };
            json!({"index":index,"role":role,"text":text,"tool_title":title,"status":state})
        }).collect();
        Self {
            rows: Arc::new(rows),
            pending,
        }
    }
}

pub fn project_snapshot(
    project: &Project,
    mut view: impl FnMut(&Workstream) -> (&'static str, Arc<Chat>),
) -> ProjectSnapshot {
    let mut snapshot = ProjectSnapshot::default();
    let mut working = 0;
    let mut needing = 0;
    let streams: Vec<_> = project.file.streams.iter().map(|s| {
        let (state, chat) = view(s);
        working += usize::from(state == "working");
        needing += usize::from(!chat.pending.is_empty());
        if let Some(id) = &s.thread_id {
            snapshot.threads.insert(id.clone(),chat.rows.clone());
            for req in &chat.pending {
                snapshot.pending.push(json!({"stream":s.id,"thread":id,"kind":req["kind"],"request_id":req["request_id"]}));
            }
        }
        let task = project.task_of(s).map(|task| {
            let phases:Vec<_> = task.phases.iter().map(|p| {
                let steps:Vec<_> = p.steps.iter().map(|s|json!({"text":s.text,"done":s.done})).collect();
                json!({"title":p.title,"steps":steps})
            }).collect();
            json!({"title":task.title,"status":task.status.word(),"phases":phases,"result":task.result})
        });
        let pool = (s.role == Role::Lead).then(|| {
            let p = project.pool(&s.id);
            json!({"ready":p.ready,"waiting":p.waiting,"running":p.running,"done":p.done,"failed":p.failed,"workers":p.workers,"total":p.total()})
        });
        json!({"id":s.id,"thread_id":s.thread_id,"name":s.name,"role":s.role,"backend":s.backend,"state":state,
            "parent":s.parent,"task":task,"pool":pool,"needs_you":chat.pending})
    }).collect();
    let notices: Vec<_> = project.notices[project.notices.len().saturating_sub(50)..]
        .iter()
        .map(|n| json!({"at":n.at,"from":n.from,"to":n.to,"text":n.text}))
        .collect();
    snapshot.summary = json!({"root":project.root.to_string_lossy(),"name":project.file.name,"autopilot":project.file.autopilot.on,
        "streams":streams.len(),"working":working,"needs_you":needing});
    snapshot.detail = snapshot.summary.clone();
    snapshot.detail["streams"] = streams.into();
    snapshot.detail["notices"] = notices.into();
    snapshot.detail["plan"] = json!(project.plan);
    snapshot
}
