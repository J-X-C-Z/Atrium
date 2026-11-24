//! The orchestration tools' handlers.
//!
//! Ported from `apps/server/src/mcp/OrchestratorMcpService.ts` and the
//! toolkit handlers in `apps/server/src/mcp/toolkits/{orchestrator,thread}/`
//! (T3 Code v0.1.11, MIT). Every mutation is one of the orchestration
//! commands decided by [`crate::orchestration`]; this module reads projections
//! and calls [`dispatch_with_limit`], never reimplementing a decision.
//!
//! Time is injected through [`Clock`]. A wait polls the projection on the
//! clock's schedule, so tests drive it with a fake clock and the server with
//! the system one.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::Value as Json;

use crate::contracts::{
    Actor, ChatAttachment, ConversationMessage, CreationSource, MessageDeliveryIntent,
    MessageDispatchMode, MessageRole, ModelSelection, ProjectId, ProviderInteractionMode,
    ProviderRequestKind, RunStatus, RuntimeMode, RuntimeRequestStatus, SettledOverride, ThreadId,
    ThreadLaunchWorkspaceStrategy, TurnItem, TurnItemStatus, is_runtime_mode_above,
};
use crate::orchestration::event_sink::Store;
use crate::orchestration::orchestrator::{DispatchResult, dispatch_with_limit};
use crate::orchestration::queued_run_order::queued_runs_in_delivery_order;
use crate::orchestration::types::{
    Command, CommandId, InitialMessage, MessageDispatchCommand, MessageId, NodeId, Projection, Run,
    RunId, RunInterruptCommand, RuntimeRequestRespond, ThreadCreateCommand,
    ThreadModelSelectionSetCommand, ThreadStopCommand, Timestamp, TurnItemBase, parse_iso,
};

use super::types::*;

/// The clock a wait uses. The engine itself never reads the wall clock.
pub trait Clock {
    fn now_ms(&self) -> i64;
    fn sleep_ms(&self, milliseconds: u64);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        jiff::Timestamp::now().as_millisecond()
    }

    fn sleep_ms(&self, milliseconds: u64) {
        std::thread::sleep(Duration::from_millis(milliseconds));
    }
}

#[derive(Clone, Debug)]
pub struct McpConfig {
    /// The project new top-level work belongs to when the caller omits one.
    pub project_id: String,
    /// The thread this MCP session serves; `None` for an outside client.
    pub thread_id: Option<ThreadId>,
    pub environment_id: String,
    pub label: String,
    pub providers: Vec<ProviderCapability>,
}

impl McpConfig {
    pub fn new(project_id: impl Into<String>, thread_id: Option<ThreadId>) -> Self {
        Self {
            project_id: project_id.into(),
            thread_id,
            environment_id: "atrium".into(),
            label: "Atrium".into(),
            providers: ProviderCapability::atrium_defaults(),
        }
    }
}

const DEFAULT_WAIT_TIMEOUT_MS: u64 = 30_000;
const MAX_WAIT_TIMEOUT_MS: u64 = 45_000;
const WAIT_POLL_MS: u64 = 50;
const DEFAULT_LIST_LIMIT: u32 = 10;
const DEFAULT_READ_LIMIT: u32 = 10;
const DEFAULT_RUN_LIMIT: u32 = 10;
const DEFAULT_ITEM_MAX_CHARS: u32 = 500;
const QUEUE_READ_MAX_CHARS: usize = 16_000;

pub struct McpService<S: Store, C: Clock> {
    store: S,
    clock: C,
    config: McpConfig,
}

impl<S: Store, C: Clock> McpService<S, C> {
    pub fn new(store: S, clock: C, config: McpConfig) -> Self {
        Self {
            store,
            clock,
            config,
        }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    pub fn config(&self) -> &McpConfig {
        &self.config
    }

    /// Point the session at the thread it serves, after a launch or when a
    /// caller outside a thread starts one.
    pub fn set_thread(&mut self, thread_id: Option<ThreadId>) {
        self.config.thread_id = thread_id;
    }

    /// Dispatch one tool call. `name` must be an orchestration tool.
    pub fn call(&mut self, name: &str, arguments: &Json) -> McpResult<Json> {
        match name {
            "orchestrator_capabilities" => to_json(self.capabilities(parse(arguments)?)?),
            "delegate_task" => to_json(self.delegate_task(parse(arguments)?)?),
            "task_status" => to_json(self.task_status(parse(arguments)?)?),
            "task_cancel" => to_json(self.cancel_task(parse(arguments)?)?),
            "t3_thread_launch" => to_json(self.thread_launch(parse(arguments)?)?),
            "t3_thread_list" => to_json(self.thread_list(parse(arguments)?)?),
            "t3_thread_read" => to_json(self.thread_read(parse(arguments)?)?),
            "t3_thread_send" => to_json(self.thread_send(parse(arguments)?)?),
            "t3_thread_wait" => to_json(self.thread_wait(parse(arguments)?)?),
            "t3_thread_interrupt" => to_json(self.thread_interrupt(parse(arguments)?)?),
            "t3_thread_configure" => to_json(self.thread_configure(parse(arguments)?)?),
            "t3_pending_request" => to_json(self.pending_request(parse(arguments)?)?),
            "t3_queue" => to_json(self.queue(parse(arguments)?)?),
            other => Err(McpError::new(
                "capability_denied",
                format!("No orchestration tool {other}."),
            )),
        }
    }

    // -----------------------------------------------------------------------
    // Capabilities
    // -----------------------------------------------------------------------

    pub fn capabilities(&self, input: CapabilitiesInput) -> McpResult<CapabilitiesResult> {
        let parent = self.parent_projection()?;
        let inherited = parent.as_ref().map(|projection| &projection.thread);
        let search = input
            .search
            .as_deref()
            .map(str::trim)
            .filter(|search| !search.is_empty())
            .map(str::to_lowercase);
        let providers = self
            .config
            .providers
            .iter()
            .cloned()
            .map(|mut provider| {
                if let Some(search) = &search {
                    let before = provider.models.len();
                    provider.models.retain(|model| {
                        model.id.to_lowercase().contains(search)
                            || model
                                .label
                                .as_deref()
                                .is_some_and(|label| label.to_lowercase().contains(search))
                    });
                    let omitted = before.saturating_sub(provider.models.len()) as u32;
                    if omitted > 0 {
                        provider.omitted_models = Some(omitted);
                    }
                }
                provider
            })
            .collect();
        Ok(CapabilitiesResult {
            parent_thread_id: inherited.map(|thread| thread.id.clone()),
            inherited_provider_instance_id: inherited
                .map(|thread| thread.model_selection.instance_id.to_string()),
            inherited_model: inherited.map(|thread| thread.model_selection.model.clone()),
            inherited_options: inherited
                .and_then(|thread| thread.model_selection.options.as_ref())
                .map(|options| options.as_slice().to_vec()),
            runtime_mode: inherited.map_or(RuntimeMode::FullAccess, |thread| thread.runtime_mode),
            interaction_mode: inherited.map_or(ProviderInteractionMode::Default, |thread| {
                thread.interaction_mode
            }),
            providers,
            features: CapabilityFeatures {
                app_owned_subagents: true,
                async_polling: true,
                cancellation: true,
                batch_thread_creation: false,
                thread_management: true,
                incremental_thread_read: true,
                scheduled_tasks: false,
                max_batch_threads: 20,
            },
        })
    }

    // -----------------------------------------------------------------------
    // Thread launch, send, read, wait, list, interrupt, configure
    // -----------------------------------------------------------------------

    pub fn thread_launch(&mut self, input: ThreadLaunchInput) -> McpResult<LaunchResult> {
        if input.title.trim().is_empty() {
            return Err(McpError::invalid("A thread needs a title."));
        }
        let project_id = input
            .project_id
            .clone()
            .unwrap_or_else(|| self.config.project_id.clone());
        let (_, model_selection, runtime_mode, interaction_mode) = self.defaults(None)?;
        let model_selection = input.model_selection.clone().unwrap_or(model_selection);
        let runtime_mode = parse_runtime_mode(input.runtime_mode.as_ref(), runtime_mode)?;
        let interaction_mode =
            parse_interaction_mode(input.interaction_mode.as_ref(), interaction_mode)?;
        let thread_id = self.operation_id(
            "thread-launch-thread",
            input.client_request_id.as_deref(),
            &project_id,
        );
        let command_id = self.operation_id(
            "thread-launch",
            input.client_request_id.as_deref(),
            &project_id,
        );
        let (branch, worktree_path, strategy) = workspace_fields(input.workspace_strategy.as_ref());
        let initial_message = input.message.as_deref().map(|text| InitialMessage {
            message_id: MessageId::new(format!("message:{}", uuid())),
            text: text.to_owned(),
            attachments: attachments(input.attachments),
            context: None,
            title_seed: Some(input.title.clone()),
            scheduled_task_id: None,
            sender_thread_id: self.config.thread_id.clone(),
            workspace_strategy: strategy,
        });
        let command = ThreadCreateCommand {
            created_by: Actor::Agent,
            creation_source: CreationSource::Mcp,
            command_id: CommandId::new(command_id),
            thread_id: ThreadId::new(thread_id.clone()),
            project_id: ProjectId::new(project_id.clone()),
            title: input.title.clone(),
            model_selection: model_selection.clone(),
            runtime_mode,
            interaction_mode,
            branch,
            worktree_path,
            link_origin: None,
            delegated_from: input.delegated_from.clone(),
            imported_native_thread: None,
            initial_message,
        };
        self.dispatch(Command::ThreadCreate(command))?;
        let projection = self.projection(&thread_id)?;
        let run = latest_run(&projection);
        Ok(LaunchResult {
            link: Some(self.link(&thread_id, &input.title)),
            project_id,
            model_selection,
            run_id: run.map(|run| run.id.clone()),
            status: run.map(|run| run.status),
            thread_id: ThreadId::new(thread_id),
        })
    }

    pub fn thread_send(&mut self, input: ThreadSendInput) -> McpResult<ThreadSendResult> {
        let thread_id = input.thread_id.clone();
        let projection = self.projection(&thread_id)?;
        let active_before = projection
            .active_run()
            .map(|run| run.id.as_str().to_owned());
        let mode = input.mode.as_deref().unwrap_or("auto");
        let (delivery_intent, dispatch_mode) = match mode {
            "auto" => (Some(MessageDeliveryIntent::Auto), None),
            "queue" => (None, Some(MessageDispatchMode::QueueAfterActive {})),
            "steer" => (Some(MessageDeliveryIntent::Steer), None),
            "restart" => (Some(MessageDeliveryIntent::Restart), None),
            other => {
                return Err(McpError::invalid(format!(
                    "Unknown send mode {other}; use auto, queue, steer or restart."
                )));
            }
        };
        let message_id = MessageId::new(self.operation_id(
            "thread-send-message",
            input.client_request_id.as_deref(),
            &thread_id,
        ));
        let command = MessageDispatchCommand {
            notification: None,
            guest_message_id: None,
            created_by: Actor::Agent,
            creation_source: CreationSource::Mcp,
            scheduled_task_id: None,
            sender_thread_id: self.config.thread_id.clone(),
            linked_sender: None,
            idle_compaction: None,
            fresh_context: input.fresh_context,
            command_id: CommandId::new(self.operation_id(
                "thread-send",
                input.client_request_id.as_deref(),
                &thread_id,
            )),
            thread_id: ThreadId::new(thread_id.clone()),
            message_id: message_id.clone(),
            text: input.message,
            context: None,
            attachments: Vec::new(),
            title_seed: None,
            model_selection: None,
            source_plan_ref: None,
            restart_continuation_of_run_id: None,
            usage_limit_continuation_of_run_id: None,
            manual_continuation_of_run_id: None,
            steer_follow_up_of_run_id: None,
            steer_follow_up_after_run_ordinal: None,
            usage_limit_recovery_request_id: None,
            delivery_intent,
            delegated_completion: None,
            dispatch_mode,
        };
        self.dispatch(Command::MessageDispatch(command))?;
        let projection = self.projection(&thread_id)?;
        let run = projection
            .runs
            .iter()
            .rev()
            .find(|run| run.user_message_id == message_id)
            .ok_or_else(|| McpError::orchestration("The message created no run."))?;
        let delivery = if run.status == RunStatus::Queued {
            "queued"
        } else if mode == "restart" && active_before.is_some() {
            "restarted"
        } else if mode == "steer" && active_before.is_some() {
            "steered"
        } else {
            "started"
        };
        Ok(ThreadSendResult {
            thread_id: ThreadId::new(thread_id.clone()),
            link: Some(self.link(&thread_id, &projection.thread.title)),
            message_id,
            run_id: run.id.clone(),
            task_id: run.delegated_task_id.clone(),
            status: run.status,
            delivery: delivery.to_owned(),
        })
    }

    pub fn thread_list(&self, input: ThreadListInput) -> McpResult<ThreadListResult> {
        let project_id = if input.all_projects == Some(true) {
            None
        } else {
            Some(
                input
                    .project_id
                    .clone()
                    .unwrap_or_else(|| self.config.project_id.clone()),
            )
        };
        let now = self.clock.now_ms();
        let statuses: Option<Vec<String>> = input
            .statuses
            .as_ref()
            .filter(|statuses| !statuses.is_empty())
            .cloned();
        let title_contains = input.title_contains.as_deref().map(str::to_lowercase);
        let mut threads = Vec::new();
        for thread_id in self.store.list_thread_ids(project_id.as_deref()) {
            let Some(projection) = self.store.projection(&thread_id) else {
                continue;
            };
            let thread = &projection.thread;
            if thread.id.as_str().is_empty() {
                continue;
            }
            if input.include_subagents == Some(false)
                && thread.lineage.relationship_to_parent
                    == Some(crate::contracts::RelationshipToParent::Subagent)
            {
                continue;
            }
            let status = thread_status(&projection);
            if statuses
                .as_ref()
                .is_some_and(|statuses| !statuses.iter().any(|candidate| candidate == &status))
            {
                continue;
            }
            if title_contains
                .as_ref()
                .is_some_and(|title_contains| !thread.title.to_lowercase().contains(title_contains))
            {
                continue;
            }
            let settled = settled(thread);
            if input.settled.is_some_and(|wanted| wanted != settled) {
                continue;
            }
            let snoozed = is_snoozed(thread, now);
            if input.snoozed.is_some_and(|wanted| wanted != snoozed) {
                continue;
            }
            threads.push(projection);
        }
        threads.sort_by(|left, right| {
            right
                .thread
                .updated_at
                .cmp(&left.thread.updated_at)
                .then_with(|| left.thread.id.as_str().cmp(right.thread.id.as_str()))
        });
        let total = threads.len() as u32;
        let cursor = input.cursor.unwrap_or(0).min(total);
        let limit = clamped(input.limit, DEFAULT_LIST_LIMIT, 100);
        let next_cursor = (cursor + limit < total).then_some(cursor + limit);
        let page: Vec<ThreadListItem> = threads
            .into_iter()
            .skip(cursor as usize)
            .take(limit as usize)
            .map(|projection| {
                let thread = &projection.thread;
                let latest = latest_run(&projection).map(|run| run.id.clone());
                ThreadListItem {
                    thread_id: thread.id.clone(),
                    project_id: thread.project_id.to_string(),
                    title: thread.title.clone(),
                    link: Some(self.link(thread.id.as_str(), &thread.title)),
                    created_by: thread.created_by,
                    creation_source: thread.creation_source,
                    status: thread_status(&projection),
                    latest_run_id: latest,
                    provider_instance_id: thread.model_selection.instance_id.to_string(),
                    model: thread.model_selection.model.clone(),
                    runtime_mode: thread.runtime_mode,
                    interaction_mode: thread.interaction_mode,
                    linked_pull_request: thread.linked_pull_request.clone(),
                    settled: settled(thread),
                    settled_at: thread.settled_at.clone(),
                    snoozed: is_snoozed(thread, now),
                    snoozed_until: thread.snoozed_until.clone(),
                    pinned: Some(thread.pinned_at.is_some()),
                    blocked_on_thread_wait: None,
                    parent_thread_id: thread.lineage.parent_thread_id.clone(),
                    relationship_to_parent: thread.lineage.relationship_to_parent,
                    delegated_from: thread.delegated_from.clone(),
                    item_count: projection.turn_items.len() as u32,
                    created_at: thread.created_at.clone(),
                    updated_at: thread.updated_at.clone(),
                }
            })
            .collect();
        Ok(ThreadListResult {
            project_id,
            current_thread_id: self.config.thread_id.clone(),
            threads: page,
            next_cursor,
            total,
        })
    }

    pub fn thread_read(&self, input: ThreadReadInput) -> McpResult<ThreadReadResult> {
        let projection = self.projection(&input.thread_id)?;
        let run_limit = clamped(input.run_limit, DEFAULT_RUN_LIMIT, 50);
        let limit = clamped(input.limit, DEFAULT_READ_LIMIT, 100);
        let max_chars = clamped(input.max_chars_per_item, DEFAULT_ITEM_MAX_CHARS, 20_000);
        let view_messages = input.view.as_deref().unwrap_or("messages") == "messages";

        let mut recent_runs: Vec<_> = projection.runs.iter().collect();
        recent_runs.sort_by_key(|run| std::cmp::Reverse(run.ordinal));
        let recent_runs: Vec<ThreadRun> = recent_runs
            .into_iter()
            .take(run_limit as usize)
            .map(|run| ThreadRun {
                run_id: run.id.clone(),
                ordinal: run.ordinal,
                status: run.status,
                provider_instance_id: run.provider_instance_id.to_string(),
                model: run.model_selection.model.clone(),
                executed_model: executed_model(&projection, run),
                requested_at: run.requested_at.clone(),
                started_at: run.started_at.clone(),
                completed_at: run.completed_at.clone(),
            })
            .collect();

        let mut rows: Vec<&TurnItem> = projection
            .turn_items
            .iter()
            .filter(|item| {
                !view_messages
                    || matches!(
                        item,
                        TurnItem::UserMessage(_) | TurnItem::AssistantMessage(_)
                    )
            })
            .collect();
        rows.sort_by_key(|item| item.base().map_or(0, |base| base.ordinal));

        let newest_first = input.before_position.is_some()
            || (input.after_position.is_none() && self.config.thread_id.is_some());
        let (page, has_more, earlier_position) = if let Some(item_id) = &input.item_id {
            let page: Vec<&TurnItem> = rows
                .iter()
                .copied()
                .filter(|item| item.base().is_some_and(|base| base.id.as_str() == item_id))
                .collect();
            (page, false, None)
        } else if let Some(after) = input.after_position {
            let remaining: Vec<&TurnItem> = rows
                .iter()
                .copied()
                .filter(|item| {
                    item.base()
                        .is_some_and(|base| (base.ordinal as i64) > after)
                })
                .collect();
            let has_more = remaining.len() > limit as usize;
            (
                remaining.into_iter().take(limit as usize).collect(),
                has_more,
                None,
            )
        } else if newest_first {
            let before = input.before_position.unwrap_or(u32::MAX);
            let remaining: Vec<&TurnItem> = rows
                .iter()
                .copied()
                .filter(|item| item.base().is_some_and(|base| base.ordinal < before))
                .collect();
            let has_more = remaining.len() > limit as usize;
            let start = remaining.len().saturating_sub(limit as usize);
            // Passing this page's first position as `beforePosition` returns
            // the page above it without repeating that item.
            let earlier = (start > 0)
                .then(|| {
                    remaining
                        .get(start)
                        .and_then(|item| item.base().map(|base| base.ordinal))
                })
                .flatten();
            (
                remaining.into_iter().skip(start).collect(),
                has_more,
                Some(earlier),
            )
        } else {
            let remaining: Vec<&TurnItem> = rows
                .iter()
                .copied()
                .filter(|item| {
                    input
                        .before_position
                        .is_none_or(|before| item.base().is_some_and(|base| base.ordinal < before))
                })
                .collect();
            let has_more = remaining.len() > limit as usize;
            (
                remaining.into_iter().take(limit as usize).collect(),
                has_more,
                Some(None),
            )
        };

        let offset = input.text_offset.unwrap_or(0);
        let items: Vec<TimelineItem> = page
            .iter()
            .map(|item| self.timeline_item(&projection, item, offset, max_chars))
            .collect();
        let next_position = items.last().map(|item| item.position);
        let earlier_position = if page.is_empty() {
            Some(None)
        } else {
            earlier_position
        };
        Ok(ThreadReadResult {
            thread: self.thread_detail(&projection),
            recent_runs,
            items,
            next_position,
            has_more,
            earlier_position,
        })
    }

    pub fn thread_wait(&self, input: ThreadWaitInput) -> McpResult<ThreadWaitResult> {
        let deadline = self.clock.now_ms() as u64 + clamp_timeout(input.timeout_ms);
        let run_id = input.run_id.clone();
        loop {
            let projection = self.projection(&input.thread_id)?;
            let run = match &run_id {
                Some(run_id) => projection.run(run_id).ok_or_else(|| {
                    McpError::new("run_not_found", format!("Run {run_id} was not found."))
                })?,
                None => {
                    match latest_run(&projection).or_else(|| projection.latest_executed_run()) {
                        Some(run) => run,
                        // An idle thread is already where the caller wants it.
                        None => {
                            return Ok(ThreadWaitResult {
                                thread_id: ThreadId::new(input.thread_id),
                                run_id: None,
                                status: "idle".into(),
                                timed_out: false,
                                next: None,
                                result_run: None,
                            });
                        }
                    }
                }
            };
            let settled = if input.until_delegated_work_settled == Some(true) {
                !projection
                    .runs
                    .iter()
                    .any(|candidate| candidate.id != run.id && candidate.status.is_active())
            } else {
                true
            };
            if run.status.is_terminal() && settled {
                // `resultRun` is the "until delegated work settled" answer; a
                // plain wait reports only the run it waited for.
                let result_run = input
                    .until_delegated_work_settled
                    .filter(|until| *until)
                    .and_then(|_| {
                        latest_terminal_run(&projection).map(|run| ResultRun {
                            run_id: run.id.clone(),
                            status: run.status,
                        })
                    });
                return Ok(ThreadWaitResult {
                    thread_id: ThreadId::new(input.thread_id),
                    run_id: Some(run.id.clone()),
                    status: run_status(run.status),
                    timed_out: false,
                    next: None,
                    result_run,
                });
            }
            let now = self.clock.now_ms() as u64;
            if now >= deadline {
                return Ok(ThreadWaitResult {
                    thread_id: ThreadId::new(input.thread_id),
                    run_id: Some(run.id.clone()),
                    status: run_status(run.status),
                    timed_out: true,
                    next: Some(RUNNING_TASK_NEXT.to_owned()),
                    result_run: None,
                });
            }
            self.clock
                .sleep_ms(WAIT_POLL_MS.min(deadline.saturating_sub(now)));
        }
    }

    pub fn thread_interrupt(
        &mut self,
        input: ThreadInterruptInput,
    ) -> McpResult<ThreadInterruptResult> {
        let projection = self.projection(&input.thread_id)?;
        let target_run = match &input.run_id {
            Some(run_id) => projection.run(run_id).cloned(),
            None => projection.active_run().cloned(),
        };
        if input.stop == Some(true) {
            let command = ThreadStopCommand {
                command_id: CommandId::new(self.operation_id(
                    "thread-stop",
                    input.client_request_id.as_deref(),
                    &input.thread_id,
                )),
                thread_id: ThreadId::new(input.thread_id.clone()),
                reason: input.reason.clone(),
                stop_delegated_tasks: None,
            };
            let result = self.dispatch(Command::ThreadStop(command))?;
            let interrupted = result
                .stored_events
                .iter()
                .find_map(|stored| match &stored.event {
                    crate::contracts::DomainEvent::RunUpdated(payload)
                        if payload.payload.status == RunStatus::Interrupted =>
                    {
                        Some(payload.payload.id.clone())
                    }
                    _ => None,
                });
            return Ok(ThreadInterruptResult {
                thread_id: ThreadId::new(input.thread_id),
                run_id: interrupted
                    .clone()
                    .or_else(|| target_run.clone().map(|run| run.id)),
                status: if interrupted.is_some() || target_run.is_some() {
                    "interrupt_requested".into()
                } else {
                    "no_active_run".into()
                },
            });
        }
        let Some(run) = target_run else {
            return Ok(ThreadInterruptResult {
                thread_id: ThreadId::new(input.thread_id),
                run_id: None,
                status: "no_active_run".into(),
            });
        };
        if run.status.is_terminal() {
            return Ok(ThreadInterruptResult {
                thread_id: ThreadId::new(input.thread_id),
                run_id: Some(run.id),
                status: run_status(run.status),
            });
        }
        let command = RunInterruptCommand {
            command_id: CommandId::new(self.operation_id(
                "thread-interrupt",
                input.client_request_id.as_deref(),
                &input.thread_id,
            )),
            thread_id: ThreadId::new(input.thread_id.clone()),
            run_id: run.id.clone(),
            reason: input.reason,
            hold_queue: None,
        };
        self.dispatch(Command::RunInterrupt(command))?;
        Ok(ThreadInterruptResult {
            thread_id: ThreadId::new(input.thread_id),
            run_id: Some(run.id),
            status: "interrupt_requested".into(),
        })
    }

    pub fn thread_configure(&mut self, input: ThreadConfigureInput) -> McpResult<ConfigureResult> {
        let thread_id = input
            .thread_id
            .clone()
            .or_else(|| self.config.thread_id.as_ref().map(ToString::to_string))
            .ok_or_else(|| McpError::invalid("Pass threadId outside a thread."))?;
        let projection = self.projection(&thread_id)?;
        if let Some(model_selection) = input.model_selection {
            let command = ThreadModelSelectionSetCommand {
                command_id: CommandId::new(uuid_command("thread-configure")),
                thread_id: ThreadId::new(thread_id),
                model_selection,
            };
            let result = self.dispatch(Command::ThreadModelSelectionSet(command))?;
            return Ok(ConfigureResult::Dispatch(DispatchCommandResult {
                sequence: result.sequence,
            }));
        }
        Ok(ConfigureResult::Configuration(ThreadConfiguration {
            thread_id: projection.thread.id.clone(),
            model_selection: projection.thread.model_selection.clone(),
            runtime_mode: projection.thread.runtime_mode,
            interaction_mode: projection.thread.interaction_mode,
        }))
    }

    // -----------------------------------------------------------------------
    // Pending requests and queue
    // -----------------------------------------------------------------------

    pub fn pending_request(
        &mut self,
        input: PendingRequestInput,
    ) -> McpResult<PendingRequestResult> {
        let thread_id = self.thread_or_current(input.thread_id.clone())?;
        let projection = self.projection(&thread_id)?;
        match input.action.as_str() {
            "list" => Ok(PendingRequestResult::List(PendingRequestList {
                request_ids: projection
                    .runtime_requests
                    .iter()
                    .filter(|request| {
                        request.status == RuntimeRequestStatus::Pending
                            && request.kind == crate::contracts::RuntimeRequestKind::UserInput
                    })
                    .map(|request| request.id.to_string())
                    .collect(),
            })),
            "read" => {
                let request_id = input
                    .request_id
                    .as_deref()
                    .ok_or_else(|| McpError::invalid("read needs requestId."))?;
                let request = projection
                    .runtime_requests
                    .iter()
                    .find(|request| request.id.as_str() == request_id)
                    .ok_or_else(|| {
                        McpError::new(
                            "invalid_request",
                            format!("Request {request_id} was not found."),
                        )
                    })?;
                Ok(PendingRequestResult::Read(PendingRequestRead {
                    request_id: request.id.to_string(),
                    // Questions live in the provider's request payload, which
                    // the engine does not project yet; the answer map is.
                    questions: Vec::new(),
                }))
            }
            "respond" => {
                let request_id = input
                    .request_id
                    .as_deref()
                    .ok_or_else(|| McpError::invalid("respond needs requestId."))?;
                let request = projection
                    .runtime_requests
                    .iter()
                    .find(|request| request.id.as_str() == request_id)
                    .ok_or_else(|| {
                        McpError::new(
                            "invalid_request",
                            format!("Request {request_id} was not found."),
                        )
                    })?;
                if is_approval_kind(request.kind) {
                    return Err(McpError::new(
                        "invalid_request",
                        "t3_pending_request cannot approve a permission request.",
                    ));
                }
                let command = RuntimeRequestRespond {
                    command_id: CommandId::new(uuid_command("pending-respond")),
                    thread_id: ThreadId::new(thread_id),
                    request_id: request.id.clone(),
                    decision: None,
                    feedback: None,
                    answers: input.answers.or_else(|| Some(BTreeMap::new())),
                    attachments_by_question_id: None,
                };
                let result = self.dispatch(Command::RuntimeRequestRespond(command))?;
                Ok(PendingRequestResult::Dispatch(DispatchCommandResult {
                    sequence: result.sequence,
                }))
            }
            other => Err(McpError::invalid(format!(
                "Unknown pending action {other}."
            ))),
        }
    }

    pub fn queue(&mut self, input: QueueInput) -> McpResult<QueueResult> {
        let thread_id = self.thread_or_current(input.thread_id.clone())?;
        let projection = self.projection(&thread_id)?;
        match input.action.as_str() {
            "list" => {
                let queued = queued_runs_in_delivery_order(&projection);
                let total = queued.len() as u32;
                let cursor = input.cursor.unwrap_or(0).min(total);
                let limit = clamped(input.limit, DEFAULT_LIST_LIMIT, 100);
                let entries: Vec<QueueEntry> = queued
                    .into_iter()
                    .skip(cursor as usize)
                    .take(limit as usize)
                    .map(|run| queue_entry(&projection, &run, usize::MAX))
                    .collect();
                Ok(QueueResult::List(QueueListResult {
                    items: entries,
                    next_cursor: (cursor + limit < total).then_some(cursor + limit),
                }))
            }
            "read" => {
                let run_id = input
                    .queued_run_id
                    .as_deref()
                    .ok_or_else(|| McpError::invalid("read needs queuedRunId."))?;
                let run = queued_run(&projection, run_id)?;
                Ok(QueueResult::Entry(queue_entry(
                    &projection,
                    &run,
                    QUEUE_READ_MAX_CHARS,
                )))
            }
            "edit" => {
                let run_id = input
                    .queued_run_id
                    .as_deref()
                    .ok_or_else(|| McpError::invalid("edit needs queuedRunId."))?;
                let text = input
                    .text
                    .clone()
                    .ok_or_else(|| McpError::invalid("edit needs text."))?;
                let command = Command::QueuedRunEdit {
                    command_id: CommandId::new(uuid_command("queue-edit")),
                    thread_id: ThreadId::new(thread_id),
                    run_id: RunId::new(run_id),
                    text,
                    attachments: None,
                };
                self.dispatch_map(command)
            }
            "cancel" => {
                let run_id = input
                    .queued_run_id
                    .as_deref()
                    .ok_or_else(|| McpError::invalid("cancel needs queuedRunId."))?;
                let command = Command::QueuedRunCancel {
                    command_id: CommandId::new(uuid_command("queue-cancel")),
                    thread_id: ThreadId::new(thread_id),
                    run_id: RunId::new(run_id),
                };
                self.dispatch_map(command)
            }
            "reorder" => {
                let run_id = input
                    .queued_run_id
                    .as_deref()
                    .ok_or_else(|| McpError::invalid("reorder needs queuedRunId."))?;
                let command = Command::QueuedRunReorder {
                    command_id: CommandId::new(uuid_command("queue-reorder")),
                    thread_id: ThreadId::new(thread_id),
                    run_id: RunId::new(run_id),
                    before_run_id: input.before_run_id.clone().flatten().map(RunId::new),
                };
                self.dispatch_map(command)
            }
            "promote_to_steer" => Err(McpError::new(
                "invalid_request",
                "promote_to_steer is not decided by this build's orchestration core.",
            )),
            other => Err(McpError::invalid(format!("Unknown queue action {other}."))),
        }
    }

    fn dispatch_map(&mut self, command: Command) -> McpResult<QueueResult> {
        let result = self.dispatch(command)?;
        Ok(QueueResult::Dispatch(DispatchCommandResult {
            sequence: result.sequence,
        }))
    }

    // -----------------------------------------------------------------------
    // Delegated tasks
    // -----------------------------------------------------------------------

    pub fn delegate_task(&mut self, input: DelegateTaskInput) -> McpResult<DelegateTaskResult> {
        let parent = self.parent_projection()?;
        let parent_thread = parent.as_ref().map(|projection| &projection.thread);
        let target = parse_target(input.target.as_ref())?;
        if let Some(project_id) = target
            .as_ref()
            .and_then(|target| target.project_id.as_ref())
            && project_id != &self.config.project_id
        {
            return Err(McpError::invalid(
                "A local task runs in this project. Omit target.projectId.",
            ));
        }
        let inherited_mode =
            parent_thread.map_or(RuntimeMode::FullAccess, |thread| thread.runtime_mode);
        let runtime_mode = parse_runtime_mode(input.runtime_mode.as_ref(), inherited_mode)?;
        if is_runtime_mode_above(runtime_mode, Some(inherited_mode)) {
            return Err(McpError::new(
                "runtime_mode_escalation_denied",
                "A delegated task cannot run with a broader runtime mode than this thread.",
            ));
        }
        let inherited_interaction = parent_thread
            .map_or(ProviderInteractionMode::Default, |thread| {
                thread.interaction_mode
            });
        let interaction_mode =
            parse_interaction_mode(input.interaction_mode.as_ref(), inherited_interaction)?;
        if inherited_interaction == ProviderInteractionMode::Default
            && interaction_mode == ProviderInteractionMode::Plan
        {
            return Err(McpError::new(
                "interaction_mode_escalation_denied",
                "A delegated task cannot run in plan mode when this thread is not.",
            ));
        }
        let model_selection = target_model_selection(target.as_ref(), parent_thread)
            .or_else(|| parent_thread.map(|thread| thread.model_selection.clone()))
            .unwrap_or_else(|| ModelSelection::new("codex", "default"));
        let scope = self.scope_id();
        let child_thread_id = match &input.client_request_id {
            Some(request) => format!("thread:mcp:{scope}:delegate:{request}"),
            None => format!("thread:{}", uuid()),
        };
        let command_id = match &input.client_request_id {
            Some(request) => format!("command:mcp:{scope}:delegate-task:{request}"),
            None => uuid_command("delegate-task"),
        };
        let title = derived_title(input.title.as_deref(), &input.task);
        let workspace_strategy = (input.isolation.as_deref() == Some("worktree")).then(|| {
            ThreadLaunchWorkspaceStrategy::Worktree {
                base_ref: "HEAD".into(),
                branch: None,
                start_from_origin: Some(false),
            }
        });
        let initial_message = InitialMessage {
            message_id: MessageId::new(format!("message:{}", uuid())),
            text: input.task.clone(),
            attachments: Vec::new(),
            context: None,
            title_seed: Some(title.clone()),
            scheduled_task_id: None,
            sender_thread_id: self.config.thread_id.clone(),
            workspace_strategy,
        };
        let create = ThreadCreateCommand {
            created_by: Actor::Agent,
            creation_source: CreationSource::Mcp,
            command_id: CommandId::new(command_id),
            thread_id: ThreadId::new(child_thread_id.clone()),
            project_id: ProjectId::new(self.config.project_id.clone()),
            title,
            model_selection,
            runtime_mode,
            interaction_mode,
            branch: None,
            worktree_path: None,
            link_origin: None,
            delegated_from: None,
            imported_native_thread: None,
            initial_message: Some(initial_message),
        };
        self.dispatch(Command::ThreadCreate(create))?;
        let task_id = NodeId::new(child_thread_id.clone());
        if input.mode.as_deref() == Some("wait") {
            let deadline = self.clock.now_ms() as u64 + clamp_timeout(input.timeout_ms);
            loop {
                let mut result = self.read_task(&task_id)?;
                if task_ends_wait(&result) {
                    result.wait_timed_out = false;
                    return Ok(result);
                }
                let now = self.clock.now_ms() as u64;
                if now >= deadline {
                    result.wait_timed_out = true;
                    return Ok(result);
                }
                self.clock
                    .sleep_ms(WAIT_POLL_MS.min(deadline.saturating_sub(now)));
            }
        }
        self.read_task(&task_id)
    }

    pub fn task_status(&self, input: TaskStatusInput) -> McpResult<DelegateTaskResult> {
        self.read_task(&NodeId::new(input.task_id))
    }

    pub fn cancel_task(&mut self, input: TaskCancelInput) -> McpResult<TaskCancelResult> {
        let task_id = input.task_id.clone();
        let projection = self
            .store
            .projection(&ThreadId::new(task_id.clone()))
            .ok_or_else(|| McpError::task_not_found(&task_id))?;
        if let Some(run) = projection.active_run() {
            let command = ThreadStopCommand {
                command_id: CommandId::new(self.operation_id(
                    "task-cancel",
                    input.client_request_id.as_deref(),
                    &task_id,
                )),
                thread_id: ThreadId::new(task_id.clone()),
                reason: input.reason,
                stop_delegated_tasks: Some(false),
            };
            self.dispatch(Command::ThreadStop(command))?;
            let _ = run;
            return Ok(TaskCancelResult {
                task_id: NodeId::new(task_id),
                status: "cancel_requested".into(),
            });
        }
        let status = latest_run(&projection).map_or("cancelled".to_owned(), |run| {
            task_status_for_run(run.status)
        });
        Ok(TaskCancelResult {
            task_id: NodeId::new(task_id),
            status,
        })
    }

    fn read_task(&self, task_id: &NodeId) -> McpResult<DelegateTaskResult> {
        let thread_id = ThreadId::new(task_id.as_str());
        let projection = self
            .store
            .projection(&thread_id)
            .ok_or_else(|| McpError::task_not_found(task_id.as_str()))?;
        let terminal = latest_terminal_run(&projection);
        // The latest run is the task's current state; an older terminal run
        // only supplies the last result.
        let chosen = latest_run(&projection)
            .or_else(|| projection.active_run())
            .ok_or_else(|| McpError::task_not_found(task_id.as_str()))?;
        let status = task_status_for_run(chosen.status);
        let work_state = if chosen.status.is_terminal() {
            "result_available"
        } else {
            "working"
        };
        let summary = terminal.and_then(|run| run_summary(&projection, &run.id));
        let pending = projection
            .runtime_requests
            .iter()
            .filter(|request| request.status == RuntimeRequestStatus::Pending)
            .collect::<Vec<_>>();
        let has_pending_question = pending
            .iter()
            .any(|request| request.kind == crate::contracts::RuntimeRequestKind::UserInput);
        let pending_approval = pending.iter().find_map(|request| {
            if !is_approval_kind(request.kind) {
                return None;
            }
            let run_id = projection
                .nodes
                .iter()
                .find(|node| node.id == request.node_id)
                .and_then(|node| node.run_id.clone())?;
            Some(PendingApproval {
                request_id: request.id.to_string(),
                kind: provider_request_kind(request.kind)?,
                run_id,
            })
        });
        let child_run_id = chosen.id.clone();
        let child_node_id = chosen
            .root_node_id
            .clone()
            .unwrap_or_else(|| task_id.clone());
        Ok(DelegateTaskResult {
            task_id: task_id.clone(),
            child_thread_id: Some(thread_id.clone()),
            remote_child: None,
            link: Some(self.link(thread_id.as_str(), &projection.thread.title)),
            child_run_id: Some(child_run_id),
            child_node_id,
            status,
            work_state: work_state.into(),
            has_pending_child_runs: projection.runs.iter().any(|run| run.status.is_active()),
            latest_terminal_run_id: terminal.map(|run| run.id.clone()),
            latest_terminal_status: terminal.map(|run| task_status_for_run(run.status)),
            latest_terminal_summary: summary.clone(),
            latest_terminal_result_context_transfer_id: None,
            provider_instance_id: chosen.provider_instance_id.to_string(),
            model: Some(chosen.model_selection.model.clone()),
            executed_model: executed_model(&projection, chosen),
            options: chosen
                .model_selection
                .options
                .as_ref()
                .map(|options| options.as_slice().to_vec()),
            summary,
            result_context_transfer_id: None,
            has_pending_question,
            pending_approval,
            wait_timed_out: false,
            next: (!chosen.status.is_terminal()).then(|| RUNNING_TASK_NEXT.to_owned()),
        })
    }

    // -----------------------------------------------------------------------
    // Shared helpers
    // -----------------------------------------------------------------------

    fn parent_projection(&self) -> McpResult<Option<Projection>> {
        Ok(self
            .config
            .thread_id
            .as_ref()
            .and_then(|thread_id| self.store.projection(thread_id)))
    }

    fn defaults(
        &self,
        parent: Option<&Projection>,
    ) -> McpResult<(
        Option<ThreadId>,
        ModelSelection,
        RuntimeMode,
        ProviderInteractionMode,
    )> {
        let owned = if parent.is_none() {
            self.parent_projection()?
        } else {
            None
        };
        let parent = parent.or(owned.as_ref());
        let thread = parent.map(|projection| &projection.thread);
        let model = thread
            .map(|thread| thread.model_selection.clone())
            .or_else(|| {
                self.config
                    .providers
                    .iter()
                    .flat_map(|provider| {
                        provider.models.iter().map(move |model| {
                            ModelSelection::new(
                                provider.provider_instance_id.clone(),
                                model.id.clone(),
                            )
                        })
                    })
                    .next()
            })
            .unwrap_or_else(|| ModelSelection::new("codex", "default"));
        Ok((
            thread.map(|thread| thread.id.clone()),
            model,
            thread.map_or(RuntimeMode::FullAccess, |thread| thread.runtime_mode),
            thread.map_or(ProviderInteractionMode::Default, |thread| {
                thread.interaction_mode
            }),
        ))
    }

    fn thread_or_current(&self, thread_id: Option<String>) -> McpResult<String> {
        thread_id
            .or_else(|| self.config.thread_id.as_ref().map(ToString::to_string))
            .ok_or_else(|| McpError::invalid("Pass threadId outside a thread."))
    }

    fn scope_id(&self) -> String {
        self.config
            .thread_id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| self.config.project_id.clone())
    }

    fn operation_id(&self, operation: &str, request: Option<&str>, subject: &str) -> String {
        match request {
            Some(request) => format!("command:mcp:{}:{operation}:{request}", self.scope_id()),
            None => {
                let _ = subject;
                uuid_command(operation)
            }
        }
    }

    fn projection(&self, thread_id: &str) -> McpResult<Projection> {
        self.store
            .projection(&ThreadId::new(thread_id))
            .ok_or_else(|| McpError::thread_not_found(thread_id))
    }

    fn dispatch(&mut self, command: Command) -> McpResult<DispatchResult> {
        let now: Timestamp = self.clock.now_ms();
        dispatch_with_limit(&mut self.store, &command, None, now)
            .map_err(|error| McpError::orchestration(error.to_string()))
    }

    fn link(&self, thread_id: &str, title: &str) -> String {
        let label = sanitized_label(title);
        let href = if self.config.environment_id.is_empty() {
            format!("t3-thread://v1/{thread_id}")
        } else {
            format!("t3-thread://v2/{}/{thread_id}", self.config.environment_id)
        };
        if label.is_empty() {
            format!("<{href}>")
        } else {
            format!("[{label}]({href})")
        }
    }

    fn thread_detail(&self, projection: &Projection) -> ThreadDetail {
        let thread = &projection.thread;
        let latest = latest_run(projection);
        let active = projection.active_run();
        ThreadDetail {
            thread_id: thread.id.clone(),
            project_id: thread.project_id.to_string(),
            title: thread.title.clone(),
            link: Some(self.link(thread.id.as_str(), &thread.title)),
            created_by: thread.created_by,
            creation_source: thread.creation_source,
            status: thread_status(projection),
            latest_run_id: latest.map(|run| run.id.clone()),
            active_run_id: active.map(|run| run.id.clone()),
            provider_instance_id: thread.model_selection.instance_id.to_string(),
            model: thread.model_selection.model.clone(),
            runtime_mode: thread.runtime_mode,
            interaction_mode: thread.interaction_mode,
            linked_pull_request: thread.linked_pull_request.clone(),
            title_regeneration: thread
                .title_regeneration
                .as_ref()
                .and_then(|marker| serde_json::to_value(marker).ok()),
            last_title_regeneration: thread
                .last_title_regeneration
                .as_ref()
                .and_then(|marker| serde_json::to_value(marker).ok()),
            branch: thread.branch.clone(),
            worktree_path: thread.worktree_path.clone(),
            parent_thread_id: thread.lineage.parent_thread_id.clone(),
            relationship_to_parent: thread.lineage.relationship_to_parent,
            delegated_from: thread.delegated_from.clone(),
            run_count: projection.runs.len() as u32,
            item_count: projection.turn_items.len() as u32,
            pending_request_count: projection
                .runtime_requests
                .iter()
                .filter(|request| request.status == RuntimeRequestStatus::Pending)
                .count() as u32,
            archived: thread.archived_at.is_some(),
            settled: settled(thread),
            settled_at: thread.settled_at.clone(),
            snoozed: is_snoozed(thread, self.clock.now_ms()),
            snoozed_until: thread.snoozed_until.clone(),
            pinned: Some(thread.pinned_at.is_some()),
            blocked_on_thread_wait: None,
            created_at: thread.created_at.clone(),
            updated_at: thread.updated_at.clone(),
        }
    }

    fn timeline_item(
        &self,
        projection: &Projection,
        item: &TurnItem,
        offset: u32,
        max_chars: u32,
    ) -> TimelineItem {
        let base = item.base().cloned().unwrap_or_default();
        let text = turn_item_text(item);
        let (text, text_truncated, next_text_offset) =
            page_text(text, offset as usize, max_chars as usize);
        let message_id = match item {
            TurnItem::UserMessage(item) => Some(item.message_id.clone()),
            TurnItem::AssistantMessage(item) => Some(item.message_id.clone()),
            _ => None,
        };
        let message: Option<&ConversationMessage> = message_id
            .as_ref()
            .and_then(|id| projection.messages.iter().find(|message| &message.id == id));
        let subagent_id = match item {
            TurnItem::Subagent(subagent) => Some(subagent.subagent_id.clone()),
            _ => None,
        };
        TimelineItem {
            position: base.ordinal,
            visibility: "local".into(),
            source_thread_id: projection.thread.id.clone(),
            item_id: base.id.clone(),
            subagent_id,
            run_id: base.run_id.clone(),
            message_id,
            created_by: message.map(|message| message.created_by),
            creation_source: message.map(|message| message.creation_source),
            sender_thread_id: message.and_then(|message| message.sender_thread_id.clone()),
            item_type: item.type_name().to_owned(),
            status: serde_json::to_value(base.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| turn_status_string(base.status)),
            title: base.title.clone(),
            text,
            text_truncated,
            next_text_offset,
            updated_at: base.updated_at.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

fn parse<T: DeserializeOwned>(arguments: &Json) -> McpResult<T> {
    serde_json::from_value(arguments.clone())
        .map_err(|error| McpError::invalid(format!("Invalid arguments: {error}")))
}

fn to_json<T: serde::Serialize>(value: T) -> McpResult<Json> {
    serde_json::to_value(value).map_err(|error| McpError::orchestration(error.to_string()))
}

fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn uuid_command(operation: &str) -> String {
    format!("command:mcp:{operation}:{}", uuid())
}

fn clamp_timeout(timeout_ms: Option<f64>) -> u64 {
    timeout_ms
        .filter(|value| value.is_finite())
        .map(|value| value.max(1.0) as u64)
        .unwrap_or(DEFAULT_WAIT_TIMEOUT_MS)
        .min(MAX_WAIT_TIMEOUT_MS)
}

fn attachments(values: Vec<Json>) -> Vec<ChatAttachment> {
    values
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect()
}

fn workspace_fields(
    strategy: Option<&ThreadLaunchWorkspaceStrategy>,
) -> (
    Option<String>,
    Option<String>,
    Option<ThreadLaunchWorkspaceStrategy>,
) {
    match strategy {
        None => (None, None, None),
        Some(ThreadLaunchWorkspaceStrategy::Root { branch }) => (branch.clone(), None, None),
        Some(ThreadLaunchWorkspaceStrategy::ExistingWorktree {
            worktree_path,
            branch,
        }) => (branch.clone(), Some(worktree_path.clone()), None),
        Some(strategy @ ThreadLaunchWorkspaceStrategy::Worktree { .. }) => {
            (None, None, Some(strategy.clone()))
        }
    }
}

fn target_model_selection(
    target: Option<&TargetInput>,
    parent: Option<&crate::contracts::AppThread>,
) -> Option<ModelSelection> {
    let target = target?;
    let instance = target
        .provider_instance_id
        .clone()
        .or_else(|| target.driver_kind.clone());
    match (instance, &target.model) {
        (Some(instance_id), Some(model)) => Some(ModelSelection {
            instance_id: crate::contracts::ProviderInstanceId::new(instance_id),
            model: model.clone(),
            options: target.options.clone(),
        }),
        _ => parent.map(|thread| thread.model_selection.clone()),
    }
}

fn derived_title(title: Option<&str>, task: &str) -> String {
    let base = title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .or_else(|| {
            task.lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
        })
        .unwrap_or("Delegated task");
    let mut clipped: String = base.chars().take(72).collect();
    if base.chars().count() > 72 {
        clipped.push_str("...");
    }
    clipped
}

fn sanitized_label(label: &str) -> String {
    label
        .chars()
        .map(|character| match character {
            '[' | ']' | '\\' | '\n' | '\r' => ' ',
            character => character,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn latest_run(projection: &Projection) -> Option<&Run> {
    projection.runs.iter().max_by_key(|run| run.ordinal)
}

fn latest_terminal_run(projection: &Projection) -> Option<&Run> {
    projection
        .runs
        .iter()
        .filter(|run| run.status.is_terminal())
        .max_by_key(|run| run.ordinal)
}

fn settled(thread: &crate::contracts::AppThread) -> bool {
    thread.settled_override == Some(SettledOverride::Settled)
        || (thread.settled_at.is_some() && thread.settled_override != Some(SettledOverride::Active))
}

fn is_snoozed(thread: &crate::contracts::AppThread, now: i64) -> bool {
    thread
        .snoozed_until
        .as_deref()
        .map(parse_iso)
        .is_some_and(|until| until > now)
}

fn thread_status(projection: &Projection) -> String {
    if let Some(run) = projection.active_run() {
        return run_status(run.status);
    }
    latest_run(projection).map_or_else(|| "idle".into(), |run| run_status(run.status))
}

fn run_status(status: RunStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| run_status_string(status))
}

fn run_status_string(status: RunStatus) -> String {
    match status {
        RunStatus::Preparing => "preparing",
        RunStatus::Queued => "queued",
        RunStatus::Starting => "starting",
        RunStatus::Running => "running",
        RunStatus::Waiting => "waiting",
        RunStatus::Completed => "completed",
        RunStatus::Interrupted => "interrupted",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled => "cancelled",
        RunStatus::RolledBack => "rolled_back",
    }
    .to_owned()
}

fn turn_status_string(status: TurnItemStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "idle".to_owned())
}

fn task_status_for_run(status: RunStatus) -> String {
    match status {
        RunStatus::Queued => "queued",
        RunStatus::Preparing => "starting",
        RunStatus::Starting => "starting",
        RunStatus::Running => "running",
        RunStatus::Waiting => "waiting",
        RunStatus::Completed => "completed",
        RunStatus::Failed => "failed",
        RunStatus::Cancelled | RunStatus::RolledBack => "cancelled",
        RunStatus::Interrupted => "interrupted",
    }
    .to_owned()
}

fn task_ends_wait(result: &DelegateTaskResult) -> bool {
    result.has_pending_question
        || result.pending_approval.is_some()
        || matches!(
            result.status.as_str(),
            "completed" | "failed" | "cancelled" | "interrupted"
        )
}

fn executed_model(projection: &Projection, run: &Run) -> Option<Option<String>> {
    let attempt = run.active_attempt_id.as_ref()?;
    projection
        .provider_turns
        .iter()
        .filter(|turn| turn.run_attempt_id.as_ref() == Some(attempt))
        .max_by_key(|turn| turn.ordinal)
        .map(|turn| turn.model.clone())
}

fn run_summary(projection: &Projection, run_id: &RunId) -> Option<String> {
    projection
        .messages
        .iter()
        .rfind(|message| {
            message.role == MessageRole::Assistant && message.run_id.as_ref() == Some(run_id)
        })
        .map(|message| message.text.clone())
}

fn is_approval_kind(kind: crate::contracts::RuntimeRequestKind) -> bool {
    matches!(
        kind,
        crate::contracts::RuntimeRequestKind::Command
            | crate::contracts::RuntimeRequestKind::FileRead
            | crate::contracts::RuntimeRequestKind::FileChange
            | crate::contracts::RuntimeRequestKind::McpElicitation
            | crate::contracts::RuntimeRequestKind::Permission
    )
}

fn provider_request_kind(
    kind: crate::contracts::RuntimeRequestKind,
) -> Option<ProviderRequestKind> {
    Some(match kind {
        crate::contracts::RuntimeRequestKind::Command => ProviderRequestKind::Command,
        crate::contracts::RuntimeRequestKind::FileRead => ProviderRequestKind::FileRead,
        crate::contracts::RuntimeRequestKind::FileChange => ProviderRequestKind::FileChange,
        crate::contracts::RuntimeRequestKind::McpElicitation => ProviderRequestKind::McpElicitation,
        crate::contracts::RuntimeRequestKind::Permission => ProviderRequestKind::Permission,
        _ => return None,
    })
}

fn queued_run(projection: &Projection, run_id: &str) -> McpResult<Run> {
    queued_runs_in_delivery_order(projection)
        .into_iter()
        .find(|run| run.id.as_str() == run_id)
        .ok_or_else(|| {
            McpError::new(
                "run_not_found",
                format!("Queued run {run_id} was not found."),
            )
        })
}

fn queue_entry(projection: &Projection, run: &Run, max_chars: usize) -> QueueEntry {
    let text = projection
        .message(&run.user_message_id)
        .map(|message| message.text.clone())
        .unwrap_or_default();
    let (text, truncated) = clip(&text, max_chars);
    QueueEntry {
        queued_run_id: run.id.clone(),
        text,
        truncated,
    }
}

fn clip(text: &str, max_chars: usize) -> (String, bool) {
    if max_chars == usize::MAX || text.chars().count() <= max_chars {
        return (text.to_owned(), false);
    }
    (text.chars().take(max_chars).collect(), true)
}

fn page_text(
    text: Option<String>,
    offset: usize,
    max_chars: usize,
) -> (Option<String>, bool, Option<Option<u32>>) {
    let Some(text) = text else {
        return (None, false, None);
    };
    let total = text.chars().count();
    let offset = offset.min(total);
    let end = (offset + max_chars).min(total);
    let page: String = text.chars().skip(offset).take(end - offset).collect();
    let truncated = end < total;
    (Some(page), truncated, Some(truncated.then_some(end as u32)))
}

fn turn_item_text(item: &TurnItem) -> Option<String> {
    let value = serde_json::to_value(item).ok()?;
    match item.type_name() {
        "user_message" | "assistant_message" | "reasoning" => {
            value.get("text").and_then(Json::as_str).map(str::to_owned)
        }
        "proposed_plan" => value
            .get("markdown")
            .and_then(Json::as_str)
            .map(str::to_owned),
        "system_notice" | "run_interrupt_request" | "run_interrupt_result" => value
            .get("message")
            .and_then(Json::as_str)
            .map(str::to_owned),
        "error" => value
            .get("failure")
            .and_then(|failure| failure.get("message"))
            .and_then(Json::as_str)
            .map(str::to_owned),
        "compaction" => value
            .get("summary")
            .and_then(Json::as_str)
            .map(str::to_owned),
        "command_execution" => {
            let input = value
                .get("input")
                .and_then(Json::as_str)
                .unwrap_or_default();
            let output = value.get("output").and_then(Json::as_str);
            Some(match output {
                Some(output) => format!("$ {input}\n{output}"),
                None => format!("$ {input}"),
            })
        }
        "file_change" => {
            let name = value
                .get("fileName")
                .and_then(Json::as_str)
                .unwrap_or_default();
            let diff = value
                .get("diffStr")
                .or_else(|| value.get("newStr"))
                .and_then(Json::as_str)
                .unwrap_or_default();
            Some(format!("{name}\n{diff}"))
        }
        "approval_request" => value
            .get("prompt")
            .or_else(|| value.get("reason"))
            .and_then(Json::as_str)
            .map(str::to_owned),
        "subagent" => value
            .get("result")
            .or_else(|| value.get("progress"))
            .or_else(|| value.get("prompt"))
            .and_then(Json::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

/// `TurnItemBase` is the shape every item shares; exposed for tests.
pub fn item_base(item: &TurnItem) -> Option<&TurnItemBase> {
    item.base()
}

/// Re-exported so a caller can name the status without the contracts module.
pub type McpItemStatus = TurnItemStatus;
