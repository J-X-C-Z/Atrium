//! A real Workspace entity, no window or backend: archive and delete leave
//! the undo window open, and undo puts things back.

use atrium_core::ThreadMeta;
use gpui::{AppContext, TestAppContext};

use super::{Settings, Workspace};

fn thread(id: &str) -> ThreadMeta {
    ThreadMeta {
        id: id.into(),
        backend: "codex".into(),
        name: None,
        preview: "A conversation".into(),
        cwd: String::new(),
        model: None,
        created_at: 1,
        updated_at: 2,
    }
}

#[gpui::test(iterations = 1)]
fn archive_stays_in_the_list_and_can_be_undone(cx: &mut TestAppContext) {
    let ws = cx.new(|_| {
        let mut ws = Workspace::with_settings(Settings {
            install_id: "me".into(),
            ..Default::default()
        });
        ws.threads.push(thread("t1"));
        ws
    });
    ws.update(cx, |ws, cx| ws.archive("t1".into(), cx));
    let token = ws.read_with(cx, |ws, _| ws.chat_change_token);
    ws.read_with(cx, |ws, _| {
        assert!(ws.settings.is_archived("t1"));
        // Archiving keeps the conversation; the list folds it away.
        assert!(ws.threads.iter().any(|t| t.id == "t1"));
        assert!(ws.pending_chat_change.is_some());
    });
    ws.update(cx, |ws, cx| ws.undo_chat_action(token, cx));
    ws.read_with(cx, |ws, _| {
        assert!(!ws.settings.is_archived("t1"));
        assert!(ws.threads.iter().any(|t| t.id == "t1"));
        assert!(ws.pending_chat_change.is_none());
    });
}

#[gpui::test(iterations = 1)]
fn delete_keeps_the_thread_until_the_undo_window_ends(cx: &mut TestAppContext) {
    let ws = cx.new(|_| {
        let mut ws = Workspace::with_settings(Settings {
            install_id: "me".into(),
            ..Default::default()
        });
        ws.threads.push(thread("t1"));
        ws
    });
    ws.update(cx, |ws, cx| ws.delete_chat("t1".into(), cx));
    let token = ws.read_with(cx, |ws, _| ws.chat_change_token);
    ws.read_with(cx, |ws, _| {
        assert!(!ws.threads.iter().any(|t| t.id == "t1"));
        assert!(ws.pending_chat_change.is_some());
    });
    ws.update(cx, |ws, cx| ws.undo_chat_action(token, cx));
    ws.read_with(cx, |ws, _| {
        assert!(ws.threads.iter().any(|t| t.id == "t1"));
        assert!(ws.pending_chat_change.is_none());
    });

    // A changed token no longer matches a newer toast.
    ws.update(cx, |ws, cx| ws.delete_chat("t1".into(), cx));
    let first = ws.read_with(cx, |ws, _| ws.chat_change_token);
    ws.update(cx, |ws, cx| ws.delete_chat("t1".into(), cx));
    ws.read_with(cx, |ws, _| {
        // Deleting an already removed id is a no-op; the first token stays.
        assert_eq!(ws.chat_change_token, first);
    });
}

#[gpui::test(iterations = 1)]
fn a_nickname_shows_instead_of_the_title(cx: &mut TestAppContext) {
    let ws = cx.new(|_| {
        let mut ws = Workspace::with_settings(Settings {
            install_id: "me".into(),
            ..Default::default()
        });
        ws.threads.push(thread("t1"));
        ws
    });
    ws.update(cx, |ws, cx| {
        ws.set_chat_nickname("t1".into(), "  Daily driver ".into(), cx)
    });
    ws.read_with(cx, |ws, _| {
        assert_eq!(ws.chat_title("t1", "Fix the build"), "Daily driver");
    });
    ws.update(cx, |ws, cx| {
        ws.set_chat_nickname("t1".into(), " ".into(), cx)
    });
    ws.read_with(cx, |ws, _| {
        assert_eq!(ws.chat_title("t1", "Fix the build"), "Fix the build");
    });
}
