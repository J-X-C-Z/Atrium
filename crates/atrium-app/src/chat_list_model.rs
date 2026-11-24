//! The sidebar's conversation list, decided without GPUI: which section a
//! conversation belongs to, the order of the sections, what a nickname
//! shows instead of the title, and what an archive or delete can undo.
//!
//! "Working" is a turn that runs, or a conversation blocked on a request.
//! "Archived" is the user's own archive (kept in `settings.json`), not the
//! backend's: an archived conversation is still listed, folded away, and can
//! be brought back. Deleting is different: the row leaves the list and the
//! backend is told only after the undo window ends.

/// One section of the conversation list, in the order they are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// Pinned by the user; pinned wins over "Working".
    Pinned,
    /// A turn is running, or the conversation waits for an approval or answer.
    Working,
    /// Everything else.
    Recent,
    /// Archived by the user, folded at the bottom.
    Archived,
}

/// What a conversation is doing, without the component's status type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Working,
    Waiting,
    Unread(usize),
}

/// The facts grouping needs about one conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowFacts {
    /// Unix seconds, newest first inside each section.
    pub at: i64,
    pub pinned: bool,
    pub archived: bool,
    pub activity: Activity,
}

/// The section `row` belongs to.
pub fn section(row: &RowFacts) -> Section {
    if row.archived {
        Section::Archived
    } else if row.pinned {
        Section::Pinned
    } else if matches!(row.activity, Activity::Working | Activity::Waiting) {
        Section::Working
    } else {
        Section::Recent
    }
}

/// Rows by section, each section newest first; empty sections are left out.
pub fn grouped(rows: &[RowFacts]) -> Vec<(Section, Vec<usize>)> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&ix| (std::cmp::Reverse(rows[ix].at), ix));
    let mut groups = Vec::new();
    for group in [
        Section::Pinned,
        Section::Working,
        Section::Recent,
        Section::Archived,
    ] {
        let items: Vec<usize> = order
            .iter()
            .copied()
            .filter(|&ix| section(&rows[ix]) == group)
            .collect();
        if !items.is_empty() {
            groups.push((group, items));
        }
    }
    groups
}

/// What the list calls a conversation: the nickname when it has one, else the
/// thread's own title.
pub fn display_title(nickname: Option<&str>, title: &str) -> String {
    match nickname.map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => title.to_string(),
    }
}

/// The two actions the list offers beside pinning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatAction {
    Archive,
    Delete,
}

/// Whether an action changes this computer's list or asks the device that
/// owns the conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatScope {
    /// This computer owns the conversation: the action applies here.
    Local,
    /// Another device owns it: the action is a request, never a local delete.
    Remote,
}

impl ChatScope {
    pub fn of(remote: bool) -> Self {
        if remote { Self::Remote } else { Self::Local }
    }

    pub fn is_remote(self) -> bool {
        self == Self::Remote
    }
}

/// What the list did, kept until the undo window ends.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatChange {
    pub key: String,
    pub action: ChatAction,
    pub scope: ChatScope,
    /// Whether the conversation was archived before the action.
    pub was_archived: bool,
}

impl ChatChange {
    pub fn new(
        key: impl Into<String>,
        action: ChatAction,
        scope: ChatScope,
        was_archived: bool,
    ) -> Self {
        Self {
            key: key.into(),
            action,
            scope,
            was_archived,
        }
    }

    /// Undo puts an archive back to how it was. `None` when the change was a
    /// delete or a remote request.
    pub fn undo_archived(&self) -> Option<bool> {
        (self.action == ChatAction::Archive && self.scope == ChatScope::Local)
            .then_some(self.was_archived)
    }

    /// Undo puts a locally deleted conversation back in the list.
    pub fn undo_restores_thread(&self) -> bool {
        self.action == ChatAction::Delete && self.scope == ChatScope::Local
    }

    /// Undo withdraws a request to the owning device; `None` for local
    /// changes.
    pub fn undo_remote_field(&self) -> Option<&'static str> {
        self.scope.is_remote().then_some(match self.action {
            ChatAction::Archive => "archived",
            ChatAction::Delete => "deleted",
        })
    }

    /// The field of the replicated overlay this remote request writes.
    pub fn remote_field(&self) -> &'static str {
        match self.action {
            ChatAction::Archive => "archived",
            ChatAction::Delete => "deleted",
        }
    }

    /// Whether the action removes the row from this computer's list at once.
    /// A remote delete only asks the owner; it never removes a local copy.
    pub fn removes_locally(&self) -> bool {
        self.action == ChatAction::Delete && self.scope == ChatScope::Local
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(at: i64, pinned: bool, archived: bool, activity: Activity) -> RowFacts {
        RowFacts {
            at,
            pinned,
            archived,
            activity,
        }
    }

    #[test]
    fn sections_are_pinned_working_recent_archived() {
        let rows = vec![
            row(5, false, false, Activity::Idle),      // recent
            row(9, true, false, Activity::Working),    // pinned wins
            row(7, false, false, Activity::Waiting),   // working
            row(3, false, false, Activity::Unread(2)), // recent
            row(8, true, true, Activity::Idle),        // archived wins
            row(6, false, false, Activity::Working),   // working
        ];
        let groups = grouped(&rows);
        let shape: Vec<(Section, Vec<usize>)> = groups;
        assert_eq!(
            shape,
            vec![
                (Section::Pinned, vec![1]),
                (Section::Working, vec![2, 5]),
                (Section::Recent, vec![0, 3]),
                (Section::Archived, vec![4]),
            ]
        );
    }

    #[test]
    fn empty_sections_are_left_out_and_order_is_newest_first() {
        let rows = vec![
            row(10, false, false, Activity::Idle),
            row(20, false, false, Activity::Idle),
        ];
        assert_eq!(grouped(&rows), vec![(Section::Recent, vec![1, 0])]);
        assert!(grouped(&[]).is_empty());
    }

    #[test]
    fn a_nickname_replaces_the_title_but_an_empty_one_does_not() {
        assert_eq!(
            display_title(Some("Daily driver"), "Fix the build"),
            "Daily driver"
        );
        assert_eq!(display_title(Some("  "), "Fix the build"), "Fix the build");
        assert_eq!(display_title(None, "Fix the build"), "Fix the build");
    }

    #[test]
    fn remote_actions_are_requests_and_undo_withdraws_them() {
        let request = ChatChange::new("t1", ChatAction::Delete, ChatScope::of(true), false);
        assert_eq!(request.scope, ChatScope::Remote);
        assert!(!request.removes_locally());
        assert!(request.undo_remote_field().is_some());
        assert_eq!(request.remote_field(), "deleted");
        assert!(!request.undo_restores_thread());
        assert_eq!(request.undo_archived(), None);

        let local = ChatChange::new("t2", ChatAction::Delete, ChatScope::of(false), true);
        assert!(local.removes_locally());
        assert!(local.undo_restores_thread());
        assert_eq!(local.undo_remote_field(), None);

        let archive = ChatChange::new("t3", ChatAction::Archive, ChatScope::Local, false);
        assert_eq!(archive.undo_archived(), Some(false));
        assert!(!archive.undo_restores_thread());
        assert_eq!(archive.remote_field(), "archived");
    }
}
