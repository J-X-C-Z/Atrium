//! Pro-tier features against `mock-codex`, through a real child process.

use std::path::PathBuf;

use atrium_core::{AgentEvent, EntryKind, GoalStatus, Item, ItemStatus, Transcript};
use codex_client::{
    CodexClient, CodexConfig, Incoming, Prediction, StartThread, TurnRequest, translate,
};
use futures::{StreamExt, channel::mpsc::UnboundedReceiver, executor::block_on};

async fn start() -> (CodexClient, UnboundedReceiver<Incoming>) {
    let config = CodexConfig {
        binary: Some(PathBuf::from(env!("CARGO_BIN_EXE_mock-codex"))),
        ..Default::default()
    };
    CodexClient::start(config).await.expect("mock starts")
}

/// Events (translated) until `stop` says so. Answers questions with `answer`.
async fn until(
    client: &CodexClient,
    events: &mut UnboundedReceiver<Incoming>,
    mut stop: impl FnMut(&AgentEvent) -> bool,
) -> Vec<AgentEvent> {
    let mut seen = Vec::new();
    while let Some(incoming) = events.next().await {
        let event = match incoming {
            Incoming::Notification { method, params } => translate::event(&method, &params),
            Incoming::Request { id, method, params } => {
                let event = translate::request(&id, &method, &params);
                if let Some(AgentEvent::UserInputRequested(request)) = &event {
                    let first = request.questions[0].options[0].0.clone();
                    client
                        .answer_user_input(id, vec![(request.questions[0].id.clone(), vec![first])])
                        .unwrap();
                }
                event
            }
            Incoming::Closed => panic!("mock closed"),
        };
        let Some(event) = event else { continue };
        let done = stop(&event);
        seen.push(event);
        if done {
            break;
        }
    }
    seen
}

fn turn_end(thread: &str) -> impl FnMut(&AgentEvent) -> bool + '_ {
    move |e| matches!(e, AgentEvent::TurnCompleted { thread_id, .. } if thread_id == thread)
}

fn text(t: &str) -> TurnRequest {
    TurnRequest {
        text: t.into(),
        ..Default::default()
    }
}

#[test]
fn the_next_message_is_predicted_through_a_fork() {
    block_on(async {
        let (client, mut events) = start().await;
        let thread = client.thread_start(StartThread::default()).await.unwrap();
        let turn = client
            .turn_start(&thread.id, text("please test this"))
            .await
            .unwrap();
        until(&client, &mut events, turn_end(&thread.id)).await;

        let Prediction::Fork(fork) = client.request_prediction(&thread.id, &turn).await.unwrap()
        else {
            panic!("the mock answers like Codex 0.169: not implemented, so a fork predicts");
        };
        let seen = until(&client, &mut events, turn_end(&fork)).await;
        let guess = seen.iter().find_map(|e| match e {
            AgentEvent::ItemCompleted {
                thread_id,
                item: Item::Agent { text, .. },
                ..
            } if *thread_id == fork => Some(text.clone()),
            _ => None,
        });
        assert_eq!(
            guess.as_deref(),
            Some("Great, now commit the change with a clear message")
        );
        client.thread_unsubscribe(&fork).await.unwrap();
        // The fork is not one of the user's chats.
        assert!(
            client
                .thread_list(50)
                .await
                .unwrap()
                .iter()
                .all(|t| t.id != fork)
        );
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e, AgentEvent::ThreadStarted(meta) if meta.id == fork))
        );
    });
}

#[test]
fn steering_reaches_the_running_turn() {
    block_on(async {
        let (client, mut events) = start().await;
        let thread = client.thread_start(StartThread::default()).await.unwrap();
        let turn = client
            .turn_start(&thread.id, text("write something long"))
            .await
            .unwrap();
        until(&client, &mut events, |e| {
            matches!(e, AgentEvent::TurnStarted { .. })
        })
        .await;
        let steered = client
            .turn_steer(&thread.id, &turn, text("and keep it short"))
            .await
            .unwrap();
        assert_eq!(steered, turn);
        let mut transcript = Transcript::new();
        for event in until(&client, &mut events, turn_end(&thread.id)).await {
            transcript.apply(&event);
        }
        let users: Vec<&str> = transcript
            .entries()
            .iter()
            .filter(|e| matches!(&e.kind, EntryKind::Item(Item::User { .. })))
            .map(|e| e.turn_id.as_str())
            .collect();
        assert_eq!(
            users,
            vec![turn.as_str(), turn.as_str()],
            "the steer joins the running turn"
        );
        assert!(transcript.turn_answer(&turn).contains("keep it short"));
    });
}

#[test]
fn chats_revert_fork_and_search() {
    block_on(async {
        let (client, mut events) = start().await;
        let thread = client.thread_start(StartThread::default()).await.unwrap();
        let first = client
            .turn_start(&thread.id, text("first question"))
            .await
            .unwrap();
        until(&client, &mut events, turn_end(&thread.id)).await;
        let second = client
            .turn_start(&thread.id, text("second question"))
            .await
            .unwrap();
        until(&client, &mut events, turn_end(&thread.id)).await;

        let branch = client.thread_fork(&thread.id, Some(&first)).await.unwrap();
        let (_, turns) = client.thread_resume(&branch.id).await.unwrap();
        assert_eq!(
            turns.len(),
            1,
            "a branch keeps the turns up to the chosen one"
        );

        client.thread_revert(&thread.id, &second).await.unwrap();
        let (_, turns) = client.thread_resume(&thread.id).await.unwrap();
        assert_eq!(turns.len(), 1, "revert drops the turn and later ones");

        let hits = client
            .thread_search("transcript reducer", 10)
            .await
            .unwrap();
        assert!(
            hits.iter()
                .any(|(meta, snippet)| meta.title().contains("transcript") && !snippet.is_empty())
        );
    });
}

#[test]
fn goals_account_usage_and_memory() {
    block_on(async {
        let (client, mut events) = start().await;
        let thread = client.thread_start(StartThread::default()).await.unwrap();
        let goal = client
            .goal_set(
                &thread.id,
                Some("Ship the release"),
                Some(GoalStatus::Active),
                Some(200_000),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(goal.token_budget, Some(200_000));
        let paused = client
            .goal_set(&thread.id, None, Some(GoalStatus::Paused), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (paused.objective.as_str(), paused.status),
            ("Ship the release", GoalStatus::Paused)
        );
        client.goal_clear(&thread.id).await.unwrap();
        assert!(client.goal_get(&thread.id).await.unwrap().is_none());

        let (account, _) = client.account().await.unwrap();
        assert_eq!(account.and_then(|a| a.plan_label()).as_deref(), Some("Pro"));
        let limits = client.rate_limits().await.unwrap();
        assert_eq!(limits.primary.unwrap().label(), "5-hour limit");
        assert!(limits.credits.is_some());

        client.logout().await.unwrap();
        assert!(client.account().await.unwrap().0.is_none());
        assert!(
            client.rate_limits().await.is_err(),
            "usage needs an account"
        );
        let (_, url) = client.login_chatgpt().await.unwrap();
        assert!(url.starts_with("https://"));
        let seen = until(&client, &mut events, |e| {
            matches!(e, AgentEvent::LoginCompleted { .. })
        })
        .await;
        assert!(matches!(
            seen.last(),
            Some(AgentEvent::LoginCompleted { success: true, .. })
        ));
        assert!(client.account().await.unwrap().0.is_some());

        client.memory_mode_set(&thread.id, false).await.unwrap();
        assert_eq!(client.memory_status().await.unwrap(), (3, true));
    });
}

#[test]
fn questions_images_plan_mode_and_fast() {
    block_on(async {
        let (client, mut events) = start().await;
        let models = client.model_list().await.unwrap();
        assert_eq!(models[0].service_tiers[0].1, "Fast");
        let thread = client.thread_start(StartThread::default()).await.unwrap();

        client
            .turn_start(&thread.id, text("ask me about storage, then make an image"))
            .await
            .unwrap();
        let mut transcript = Transcript::new();
        for event in until(&client, &mut events, turn_end(&thread.id)).await {
            transcript.apply(&event);
        }
        assert!(
            transcript
                .entries()
                .iter()
                .any(|e| matches!(&e.kind, EntryKind::Question { .. }))
        );
        assert!(transcript.entries().iter().any(|e| matches!(
            &e.kind,
            EntryKind::Item(Item::Image { status: ItemStatus::Completed, path: Some(path), .. })
                if std::path::Path::new(path).exists()
        )));
        let last_turn = transcript.entries().last().unwrap().turn_id.clone();
        assert!(transcript.turn_answer(&last_turn).contains("SQLite"));

        client
            .turn_start(
                &thread.id,
                TurnRequest {
                    text: "add a row kind".into(),
                    model: Some("mock-1".into()),
                    plan_mode: true,
                    service_tier: Some("priority".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut transcript = Transcript::new();
        let seen = until(&client, &mut events, turn_end(&thread.id)).await;
        for event in &seen {
            transcript.apply(event);
        }
        let turn = transcript.entries().last().unwrap().turn_id.clone();
        assert!(transcript.turn_answer(&turn).contains("Proposed plan"));
        assert!(
            seen.iter()
                .any(|e| matches!(e, AgentEvent::RateLimitsUpdated(_)))
        );
    });
}
