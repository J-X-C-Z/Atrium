//! The per-chat message queue: ordering, editing, send-now and draining.
//!
//! This is the pure state machine. It has no GPUI context and no runtime: the
//! workspace calls these functions, and the tests drive the order directly.
//! Queues are persisted through [`crate::settings`] so a restart keeps them.

use std::collections::HashMap;

use crate::settings::SavedQueuedMessage;
use crate::workspace::Composed;

/// A message held until the chat's running turn ends.
#[derive(Clone, Debug, PartialEq)]
pub struct QueuedMessage {
    pub id: u64,
    pub composed: Composed,
}

impl QueuedMessage {
    pub fn saved(&self) -> SavedQueuedMessage {
        SavedQueuedMessage {
            id: self.id,
            text: self.composed.text.clone(),
            images: self.composed.images.clone(),
            mentions: self.composed.mentions.clone(),
        }
    }

    pub fn from_saved(saved: SavedQueuedMessage) -> Self {
        Self {
            id: saved.id,
            composed: Composed {
                text: saved.text,
                images: saved.images,
                mentions: saved.mentions,
            },
        }
    }
}

/// The id for a new message; the counter moves past it.
pub fn next_id(next: &mut u64) -> u64 {
    let id = *next;
    *next += 1;
    id
}

/// Holds a message at the end of `messages`.
pub fn insert(messages: &mut Vec<QueuedMessage>, message: QueuedMessage) {
    messages.push(message);
}

/// Holds a message at the front, such as one a send-now could not deliver.
pub fn insert_front(messages: &mut Vec<QueuedMessage>, message: QueuedMessage) {
    messages.insert(0, message);
}

/// Takes one message out by id.
pub fn take(messages: &mut Vec<QueuedMessage>, id: u64) -> Option<QueuedMessage> {
    let ix = messages.iter().position(|message| message.id == id)?;
    Some(messages.remove(ix))
}

/// Moves a message one place earlier (`up`) or later; returns whether it moved.
pub fn move_item(messages: &mut [QueuedMessage], id: u64, up: bool) -> bool {
    let Some(ix) = messages.iter().position(|message| message.id == id) else {
        return false;
    };
    let to = if up {
        ix.checked_sub(1)
    } else {
        Some(ix + 1).filter(|to| *to < messages.len())
    };
    let Some(to) = to else {
        return false;
    };
    messages.swap(ix, to);
    true
}

/// Takes the next message to send after a turn ends.
pub fn take_first(messages: &mut Vec<QueuedMessage>) -> Option<QueuedMessage> {
    (!messages.is_empty()).then(|| messages.remove(0))
}

/// Rebuilds queues saved with settings, keeping ids unique.
pub fn restore(
    saved: &HashMap<String, Vec<SavedQueuedMessage>>,
    next: u64,
) -> (HashMap<String, Vec<QueuedMessage>>, u64) {
    let mut next = next;
    let mut queues = HashMap::new();
    for (key, list) in saved {
        let messages: Vec<QueuedMessage> = list
            .iter()
            .cloned()
            .map(QueuedMessage::from_saved)
            .collect();
        for message in &messages {
            next = next.max(message.id.saturating_add(1));
        }
        if !messages.is_empty() {
            queues.insert(key.clone(), messages);
        }
    }
    (queues, next)
}

/// The queues as settings store them, without empty chats.
pub fn saved(
    queues: &HashMap<String, Vec<QueuedMessage>>,
    next: u64,
) -> (HashMap<String, Vec<SavedQueuedMessage>>, u64) {
    let saved = queues
        .iter()
        .filter(|(_, messages)| !messages.is_empty())
        .map(|(key, messages)| {
            (
                key.clone(),
                messages.iter().map(QueuedMessage::saved).collect(),
            )
        })
        .collect();
    (saved, next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn message(id: u64, text: &str) -> QueuedMessage {
        QueuedMessage {
            id,
            composed: Composed::text(text.into()),
        }
    }

    fn texts(messages: &[QueuedMessage]) -> Vec<&str> {
        messages
            .iter()
            .map(|message| message.composed.text.as_str())
            .collect()
    }

    #[test]
    fn messages_keep_their_order_and_move_up_and_down() {
        let mut queue = Vec::new();
        insert(&mut queue, message(0, "first"));
        insert(&mut queue, message(1, "second"));
        insert(&mut queue, message(2, "third"));
        assert_eq!(texts(&queue), vec!["first", "second", "third"]);

        assert!(move_item(&mut queue, 2, true));
        assert_eq!(texts(&queue), vec!["first", "third", "second"]);
        assert!(move_item(&mut queue, 0, false));
        assert_eq!(texts(&queue), vec!["third", "first", "second"]);
        // The ends stay put: "third" (id 2) is first, "second" (id 1) last.
        assert!(!move_item(&mut queue, 2, true));
        assert!(!move_item(&mut queue, 1, false));
        assert!(!move_item(&mut queue, 99, true));
        assert_eq!(texts(&queue), vec!["third", "first", "second"]);
    }

    #[test]
    fn editing_and_removing_take_the_named_message() {
        let mut queue = vec![message(0, "one"), message(1, "two")];
        let edit = take(&mut queue, 1).expect("queued");
        assert_eq!(edit.composed.text, "two");
        assert_eq!(texts(&queue), vec!["one"]);
        assert!(take(&mut queue, 1).is_none());
    }

    #[test]
    fn draining_sends_one_at_a_time_in_order() {
        let mut queue = vec![message(0, "one"), message(1, "two")];
        let first = take_first(&mut queue).expect("first");
        assert_eq!(first.composed.text, "one");
        assert_eq!(texts(&queue), vec!["two"]);
        let second = take_first(&mut queue).expect("second");
        assert_eq!(second.composed.text, "two");
        assert!(take_first(&mut queue).is_none());
    }

    #[test]
    fn send_now_that_cannot_deliver_goes_back_to_the_front() {
        let mut queue = vec![message(0, "one"), message(1, "two")];
        let message = take(&mut queue, 1).expect("queued");
        insert_front(&mut queue, message);
        assert_eq!(texts(&queue), vec!["two", "one"]);
    }

    #[test]
    fn queues_round_trip_through_settings_and_ids_stay_unique() {
        let mut queue = vec![QueuedMessage {
            id: 3,
            composed: Composed {
                text: "look".into(),
                images: vec![PathBuf::from("/tmp/a.png")],
                mentions: vec![PathBuf::from("/tmp/a.rs")],
            },
        }];
        queue.push(message(4, "then this"));
        let mut queues = HashMap::new();
        queues.insert("thread-1".into(), queue);
        queues.insert("empty".into(), Vec::new());
        let (saved, next) = saved(&queues, 5);
        assert!(!saved.contains_key("empty"));
        assert_eq!(next, 5);
        assert_eq!(saved["thread-1"].len(), 2);

        let (restored, next) = restore(&saved, 0);
        assert_eq!(next, 5);
        let restored = &restored["thread-1"];
        assert_eq!(restored[0].composed.text, "look");
        assert_eq!(
            restored[0].composed.images,
            vec![PathBuf::from("/tmp/a.png")]
        );
        assert_eq!(restored[1].id, 4);
    }

    #[test]
    fn ids_start_at_zero_and_move_forward() {
        let mut next = 0;
        assert_eq!(next_id(&mut next), 0);
        assert_eq!(next_id(&mut next), 1);
        assert_eq!(next, 2);
    }
}
