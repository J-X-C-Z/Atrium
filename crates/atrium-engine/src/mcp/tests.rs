//! Handler tests against the in-memory store.
//!
//! The cases mirror what T3's MCP service tests assert about tool results:
//! a launch creates one thread and run, a queued send stays queued, a read
//! pages timeline items, a delegated task is cancelled, and a pending question
//! can be answered but an approval cannot.

use std::cell::Cell;

use serde_json::json;

use super::service::{Clock, McpConfig, McpService};
use super::types::*;
use crate::contracts::{
    DomainEvent, DomainEventBase, EventId, EventPayload, NodeId, ResponseCapability, RunStatus,
    RuntimeRequest, RuntimeRequestId, RuntimeRequestKind, RuntimeRequestStatus,
};
use crate::orchestration::event_sink::{MemoryStore, Store, WriteInput};
use crate::orchestration::types::{ThreadId, iso};

struct StepClock {
    now: Cell<i64>,
}

impl StepClock {
    fn new(now: i64) -> Self {
        Self {
            now: Cell::new(now),
        }
    }
}

impl Clock for StepClock {
    fn now_ms(&self) -> i64 {
        self.now.get()
    }

    fn sleep_ms(&self, milliseconds: u64) {
        self.now
            .set(self.now.get() + milliseconds.min(1_000) as i64);
    }
}

fn service() -> McpService<MemoryStore, StepClock> {
    McpService::new(
        MemoryStore::new(),
        StepClock::new(1_000),
        McpConfig::new("project:test", None),
    )
}

fn launch_input(title: &str, message: Option<&str>) -> ThreadLaunchInput {
    ThreadLaunchInput {
        environment_id: None,
        project_id: None,
        scratch: None,
        title: title.into(),
        model_selection: None,
        runtime_mode: None,
        interaction_mode: None,
        workspace_strategy: None,
        message: message.map(str::to_owned),
        attachments: Vec::new(),
        client_request_id: None,
        delegated_from: None,
    }
}

fn send_input(thread_id: &str, text: &str, mode: &str) -> ThreadSendInput {
    ThreadSendInput {
        environment_id: None,
        thread_id: thread_id.into(),
        message: text.into(),
        mode: Some(mode.into()),
        fresh_context: None,
        client_request_id: None,
    }
}

fn read_input(thread_id: &str) -> ThreadReadInput {
    ThreadReadInput {
        environment_id: None,
        thread_id: thread_id.into(),
        item_id: None,
        text_offset: None,
        view: Some("messages".into()),
        after_position: Some(-1),
        before_position: None,
        limit: Some(20),
        run_limit: None,
        max_chars_per_item: None,
    }
}

fn delegate_input(task: &str, mode: &str, timeout_ms: Option<f64>) -> DelegateTaskInput {
    DelegateTaskInput {
        task: task.into(),
        target: None,
        title: None,
        role: None,
        isolation: None,
        mode: Some(mode.into()),
        timeout_ms,
        client_request_id: None,
        runtime_mode: None,
        interaction_mode: None,
    }
}

#[test]
fn a_launch_creates_one_thread_and_run() {
    let mut service = service();
    let launched = service
        .thread_launch(launch_input("Hello", Some("hello")))
        .expect("launch");
    assert_eq!(launched.project_id, "project:test");
    assert!(launched.run_id.is_some());
    assert_eq!(launched.status, Some(RunStatus::Starting));
    assert!(
        launched
            .link
            .as_deref()
            .is_some_and(|link| link.contains("t3-thread://v2"))
    );

    let projection = service
        .store()
        .projection(&launched.thread_id)
        .expect("projection");
    assert_eq!(projection.runs.len(), 1);
    assert_eq!(projection.messages.len(), 1);
    assert_eq!(projection.messages[0].text, "hello");
}

#[test]
fn a_send_queues_while_the_turn_is_active() {
    let mut service = service();
    let launched = service
        .thread_launch(launch_input("Hello", Some("hello")))
        .expect("launch");
    let sent = service
        .thread_send(send_input(launched.thread_id.as_str(), "later", "queue"))
        .expect("send");
    assert_eq!(sent.delivery, "queued");
    assert_eq!(sent.status, RunStatus::Queued);
    let projection = service
        .store()
        .projection(&launched.thread_id)
        .expect("projection");
    assert_eq!(projection.runs.len(), 2);
}

#[test]
fn read_pages_messages_from_the_start() {
    let mut service = service();
    let launched = service
        .thread_launch(launch_input("Hello", Some("hello")))
        .expect("launch");
    let read = service
        .thread_read(read_input(launched.thread_id.as_str()))
        .expect("read");
    assert_eq!(read.thread.thread_id, launched.thread_id);
    assert_eq!(read.thread.run_count, 1);
    let first = read.items.first().expect("an item");
    assert_eq!(first.item_type, "user_message");
    assert_eq!(first.text.as_deref(), Some("hello"));
    assert_eq!(
        read.next_position,
        read.items.last().map(|item| item.position)
    );
}

#[test]
fn waiting_on_an_idle_thread_returns_at_once() {
    let mut service = service();
    let launched = service
        .thread_launch(launch_input("Idle", None))
        .expect("launch");
    assert!(launched.run_id.is_none());
    let wait = service
        .thread_wait(ThreadWaitInput {
            environment_id: None,
            thread_id: launched.thread_id.to_string(),
            run_id: None,
            timeout_ms: Some(5_000.0),
            until_delegated_work_settled: None,
        })
        .expect("wait");
    assert_eq!(wait.status, "idle");
    assert!(!wait.timed_out);
    assert_eq!(wait.run_id, None);
}

#[test]
fn list_pages_threads_newest_first() {
    let mut service = service();
    let first = service
        .thread_launch(launch_input("First", Some("one")))
        .expect("first");
    let _second = service
        .thread_launch(launch_input("Second", Some("two")))
        .expect("second");
    let page = service
        .thread_list(ThreadListInput {
            limit: Some(1),
            ..ThreadListInput::default()
        })
        .expect("list");
    assert_eq!(page.total, 2);
    assert_eq!(page.threads.len(), 1);
    assert_eq!(page.next_cursor, Some(1));
    assert_eq!(page.current_thread_id, None);
    let _ = first;
}

#[test]
fn interrupt_requests_the_running_turn() {
    let mut service = service();
    let launched = service
        .thread_launch(launch_input("Hello", Some("hello")))
        .expect("launch");
    let interrupted = service
        .thread_interrupt(ThreadInterruptInput {
            environment_id: None,
            thread_id: launched.thread_id.to_string(),
            run_id: None,
            reason: None,
            stop: None,
            client_request_id: None,
        })
        .expect("interrupt");
    assert_eq!(interrupted.status, "interrupt_requested");
    assert_eq!(interrupted.run_id, launched.run_id);
    let projection = service
        .store()
        .projection(&launched.thread_id)
        .expect("projection");
    assert!(projection.runs[0].status.is_terminal());
}

#[test]
fn configure_reads_and_sets_the_model() {
    let mut service = service();
    let launched = service
        .thread_launch(launch_input("Hello", Some("hello")))
        .expect("launch");
    let read = service
        .thread_configure(ThreadConfigureInput {
            environment_id: None,
            thread_id: Some(launched.thread_id.to_string()),
            model_selection: None,
        })
        .expect("read");
    let ConfigureResult::Configuration(configuration) = read else {
        panic!("expected the thread's configuration");
    };
    assert_eq!(configuration.thread_id, launched.thread_id);

    let set = service
        .thread_configure(ThreadConfigureInput {
            environment_id: None,
            thread_id: Some(launched.thread_id.to_string()),
            model_selection: Some(crate::contracts::ModelSelection::new("codex", "gpt-other")),
        })
        .expect("set");
    assert!(matches!(set, ConfigureResult::Dispatch(_)));
    let projection = service
        .store()
        .projection(&launched.thread_id)
        .expect("projection");
    assert_eq!(projection.thread.model_selection.model, "gpt-other");
}

#[test]
fn capabilities_inherit_the_parent_thread() {
    let mut service = service();
    let launched = service
        .thread_launch(launch_input("Hello", Some("hello")))
        .expect("launch");
    service.set_thread(Some(launched.thread_id.clone()));
    let capabilities = service
        .capabilities(CapabilitiesInput {
            environment_id: None,
            search: None,
        })
        .expect("capabilities");
    assert_eq!(capabilities.parent_thread_id, Some(launched.thread_id));
    assert_eq!(capabilities.inherited_model.as_deref(), Some("default"));
    assert!(!capabilities.features.scheduled_tasks);
    assert!(capabilities.providers.len() >= 4);
}

#[test]
fn queue_lists_reads_edits_and_cancels() {
    let mut service = service();
    let launched = service
        .thread_launch(launch_input("Hello", Some("hello")))
        .expect("launch");
    service
        .thread_send(send_input(launched.thread_id.as_str(), "one", "queue"))
        .expect("first queue");
    service
        .thread_send(send_input(launched.thread_id.as_str(), "two", "queue"))
        .expect("second queue");
    let list = service
        .queue(QueueInput {
            action: "list".into(),
            environment_id: None,
            thread_id: Some(launched.thread_id.to_string()),
            queued_run_id: None,
            text: None,
            before_run_id: None,
            target_run_id: None,
            cursor: None,
            limit: None,
        })
        .expect("list");
    let QueueResult::List(list) = list else {
        panic!("expected a queue list");
    };
    assert_eq!(list.items.len(), 2);
    let first_run = list.items[0].queued_run_id.clone();

    let read = service
        .queue(QueueInput {
            action: "read".into(),
            environment_id: None,
            thread_id: Some(launched.thread_id.to_string()),
            queued_run_id: Some(first_run.to_string()),
            text: None,
            before_run_id: None,
            target_run_id: None,
            cursor: None,
            limit: None,
        })
        .expect("read");
    let QueueResult::Entry(entry) = read else {
        panic!("expected one queue entry");
    };
    assert_eq!(entry.text, "one");

    service
        .queue(QueueInput {
            action: "edit".into(),
            environment_id: None,
            thread_id: Some(launched.thread_id.to_string()),
            queued_run_id: Some(first_run.to_string()),
            text: Some("one, edited".into()),
            before_run_id: None,
            target_run_id: None,
            cursor: None,
            limit: None,
        })
        .expect("edit");
    let projection = service
        .store()
        .projection(&launched.thread_id)
        .expect("projection");
    let queued = projection
        .runs
        .iter()
        .find(|run| run.id == first_run)
        .expect("queued run");
    assert_eq!(
        projection
            .message(&queued.user_message_id)
            .expect("queued message")
            .text,
        "one, edited"
    );

    service
        .queue(QueueInput {
            action: "cancel".into(),
            environment_id: None,
            thread_id: Some(launched.thread_id.to_string()),
            queued_run_id: Some(first_run.to_string()),
            text: None,
            before_run_id: None,
            target_run_id: None,
            cursor: None,
            limit: None,
        })
        .expect("cancel");
    let projection = service
        .store()
        .projection(&launched.thread_id)
        .expect("projection");
    assert!(
        projection
            .runs
            .iter()
            .find(|run| run.id == first_run)
            .is_none_or(|run| run.status != RunStatus::Queued)
    );
}

#[test]
fn promote_to_steer_reports_that_it_is_unported() {
    let mut service = service();
    let launched = service
        .thread_launch(launch_input("Hello", Some("hello")))
        .expect("launch");
    let error = service
        .queue(QueueInput {
            action: "promote_to_steer".into(),
            environment_id: None,
            thread_id: Some(launched.thread_id.to_string()),
            queued_run_id: None,
            text: None,
            before_run_id: None,
            target_run_id: None,
            cursor: None,
            limit: None,
        })
        .expect_err("unported");
    assert_eq!(error.code, "invalid_request");
}

#[test]
fn a_delegated_task_is_read_and_cancelled() {
    let mut service = service();
    let delegated = service
        .delegate_task(delegate_input("do it", "async", None))
        .expect("delegate");
    assert_eq!(delegated.work_state, "working");
    assert!(delegated.child_thread_id.is_some());
    assert_eq!(delegated.status, "starting");

    let status = service
        .task_status(TaskStatusInput {
            task_id: delegated.task_id.to_string(),
        })
        .expect("status");
    assert_eq!(status.task_id, delegated.task_id);
    assert!(status.link.is_some());

    let cancelled = service
        .cancel_task(TaskCancelInput {
            task_id: delegated.task_id.to_string(),
            reason: None,
            client_request_id: None,
        })
        .expect("cancel");
    assert_eq!(cancelled.status, "cancel_requested");
}

#[test]
fn a_wait_delegation_times_out_without_cancelling() {
    let mut service = service();
    let delegated = service
        .delegate_task(delegate_input("do it", "wait", Some(5.0)))
        .expect("delegate");
    assert!(delegated.wait_timed_out);
    assert_eq!(delegated.status, "starting");
    let projection = service
        .store()
        .projection(&delegated.child_thread_id.expect("child"))
        .expect("child projection");
    assert!(!projection.runs[0].status.is_terminal());
}

#[test]
fn pending_questions_can_be_answered_but_approvals_cannot() {
    let mut service = service();
    let launched = service
        .thread_launch(launch_input("Hello", Some("hello")))
        .expect("launch");
    let projection = service
        .store()
        .projection(&launched.thread_id)
        .expect("projection");
    let run = &projection.runs[0];
    let node_id = run.root_node_id.clone().expect("root node");

    write_request(
        &mut service,
        &launched.thread_id,
        &node_id,
        "request:question",
        RuntimeRequestKind::UserInput,
    );
    write_request(
        &mut service,
        &launched.thread_id,
        &node_id,
        "request:approval",
        RuntimeRequestKind::Permission,
    );

    let listed = service
        .pending_request(PendingRequestInput {
            action: "list".into(),
            environment_id: None,
            thread_id: Some(launched.thread_id.to_string()),
            request_id: None,
            answers: None,
        })
        .expect("list");
    let PendingRequestResult::List(list) = listed else {
        panic!("expected a request list");
    };
    assert_eq!(list.request_ids, vec!["request:question".to_string()]);

    let read = service
        .pending_request(PendingRequestInput {
            action: "read".into(),
            environment_id: None,
            thread_id: Some(launched.thread_id.to_string()),
            request_id: Some("request:question".into()),
            answers: None,
        })
        .expect("read");
    let PendingRequestResult::Read(read) = read else {
        panic!("expected one request");
    };
    assert_eq!(read.request_id, "request:question");
    assert!(read.questions.is_empty());

    let approval = service
        .pending_request(PendingRequestInput {
            action: "respond".into(),
            environment_id: None,
            thread_id: Some(launched.thread_id.to_string()),
            request_id: Some("request:approval".into()),
            answers: None,
        })
        .expect_err("approvals are not answered here");
    assert_eq!(approval.code, "invalid_request");

    let mut answers = std::collections::BTreeMap::new();
    answers.insert("q1".into(), json!("yes"));
    let answered = service
        .pending_request(PendingRequestInput {
            action: "respond".into(),
            environment_id: None,
            thread_id: Some(launched.thread_id.to_string()),
            request_id: Some("request:question".into()),
            answers: Some(answers),
        })
        .expect("respond");
    assert!(matches!(answered, PendingRequestResult::Dispatch(_)));
    let projection = service
        .store()
        .projection(&launched.thread_id)
        .expect("projection");
    let request = projection
        .runtime_request("request:question")
        .expect("request");
    assert_eq!(request.status, RuntimeRequestStatus::Resolved);
    assert_eq!(
        request.answers.as_ref().expect("answers")["q1"],
        json!("yes")
    );
}

fn write_request(
    service: &mut McpService<MemoryStore, StepClock>,
    thread_id: &ThreadId,
    node_id: &NodeId,
    request_id: &str,
    kind: RuntimeRequestKind,
) {
    let request = RuntimeRequest {
        id: RuntimeRequestId::new(request_id),
        node_id: node_id.clone(),
        provider_turn_id: None,
        native_request_ref: None,
        kind,
        status: RuntimeRequestStatus::Pending,
        response_capability: ResponseCapability::Message {},
        created_at: iso(1_000),
        resolved_at: None,
        decision: None,
        feedback: None,
        answers: None,
    };
    let event = DomainEvent::RuntimeRequestUpdated(EventPayload {
        base: DomainEventBase {
            id: EventId::new(format!("event:{request_id}")),
            thread_id: thread_id.clone(),
            run_id: None,
            node_id: Some(node_id.clone()),
            driver: None,
            provider_instance_id: None,
            raw_event_id: None,
            occurred_at: iso(1_000),
        },
        payload: request,
    });
    service
        .store_mut()
        .write(WriteInput {
            command_id: None,
            events: vec![event],
            effects: Vec::new(),
            now: 1_000,
        })
        .expect("write request");
}
