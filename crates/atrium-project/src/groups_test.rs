use super::*;
use crate::{Workstream, mcp, parse_notices};
use serde_json::json;

fn stream(id: &str, role: Role, parent: Option<&str>) -> Workstream {
    Workstream {
        id: id.into(),
        role,
        parent: parent.map(Into::into),
        ..Default::default()
    }
}

fn project(name: &str) -> Project {
    let root = std::env::temp_dir().join(format!("atrium-groups-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let mut p = Project::open(&root).unwrap();
    p.file.streams = vec![
        stream("boss", Role::Controller, None),
        stream("perf", Role::Lead, Some("boss")),
        stream("perf-a", Role::Worker, Some("perf")),
        stream("perf-b", Role::Worker, Some("perf")),
        stream("web", Role::Lead, Some("boss")),
        stream("web-a", Role::Worker, Some("web")),
    ];
    p.file.streams[3].retired = true;
    p.save().unwrap();
    p
}

fn names(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

fn notice(from: &str, to: &[&str], text: &str) -> ProjectNotice {
    ProjectNotice {
        at: "2026-01-01 10:00".into(),
        from: from.into(),
        to: to.iter().map(|s| s.to_string()).collect(),
        text: text.into(),
    }
}

fn call(target: &mcp::Target, name: &str, args: serde_json::Value) -> String {
    let reply = mcp::handle(
        target,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":name,"arguments":args}}),
    )
    .unwrap();
    reply["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn automatic_groups_leave_out_retired_streams() {
    let mut p = project("auto");
    let g = p.groups();
    assert_eq!(names(g.members("leads").unwrap()), ["boss", "perf", "web"]);
    assert_eq!(names(g.members("perf").unwrap()), ["perf", "perf-a"]);
    assert_eq!(names(g.members("web").unwrap()), ["web", "web-a"]);
    p.stream_mut("web").unwrap().retired = true;
    let g = p.groups();
    assert!(g.members("web").is_none());
    assert_eq!(names(g.members("leads").unwrap()), ["boss", "perf"]);
}

#[test]
fn custom_groups_are_validated_and_stored() {
    let mut p = project("custom");
    let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(
        p.create_group("perf", "x", &ids(&["web"])).is_err(),
        "controller only"
    );
    for bad in ["leads", "all", "perf", "perf-a", "", "a b"] {
        assert!(
            p.create_group("boss", bad, &ids(&["web"])).is_err(),
            "{bad}"
        );
    }
    assert!(p.create_group("boss", "ui", &ids(&["nobody"])).is_err());
    assert!(p.create_group("boss", "ui", &[]).is_err());
    p.create_group("boss", "ui", &ids(&["web-a", "perf", "web-a", "perf-b"]))
        .unwrap();
    let text = fs::read_to_string(p.dir().join("groups.json")).unwrap();
    let on_disk: BTreeMap<String, Vec<String>> = serde_json::from_str(&text).unwrap();
    assert_eq!(on_disk["ui"], ["web-a", "perf", "perf-b"]);
    // Reopened, the group is there; retired members drop out.
    let p = Project::load(&p.root).unwrap();
    assert_eq!(names(p.groups().members("ui").unwrap()), ["web-a", "perf"]);
    assert!(p.groups_text("perf").contains("`@ui`"));
    assert!(p.groups_text("perf").contains("`@perf`"));
}

#[test]
fn a_group_notice_reaches_members_but_not_the_sender_or_others() {
    let mut p = project("reach");
    let n = notice("perf", &["@perf"], "align on the cache key");
    assert_eq!(p.recipients(&n), ["perf-a"]);
    assert!(p.notice_is_for(&n, "perf-a"));
    assert!(!p.notice_is_for(&n, "perf"));
    assert!(!p.notice_is_for(&n, "web-a"));
    assert!(!p.notice_is_for(&n, "boss"));
    let n = notice("web", &["@leads"], "x");
    assert_eq!(p.recipients(&n), ["boss", "perf"]);
    // Plain ids behave as before; `is_for` still works alone.
    let n = notice("boss", &["perf-a", "web"], "x");
    assert_eq!(p.recipients(&n), ["perf-a", "web"]);
    assert!(n.is_for("web"));
    // Workers do not get `all`, but do get their group.
    p.notices = vec![
        notice("boss", &["all"], "everyone"),
        notice("perf", &["@perf"], "group"),
    ];
    assert_eq!(p.unread("perf-a").len(), 1);
    assert_eq!(p.unread("perf-a")[0].text, "group");
    assert_eq!(p.unread("web").len(), 1);
    assert_eq!(p.unread("perf").len(), 1);
    assert_eq!(p.unread("perf")[0].text, "everyone");
}

#[test]
fn a_mention_makes_a_member_directly_addressed() {
    let p = project("mention");
    let plain = notice("perf-a", &["@perf"], "heads up");
    assert!(p.notice_is_for(&plain, "perf"));
    assert!(
        !p.notice_wakes(&plain, "perf"),
        "a group message does not wake"
    );
    let said = notice("perf", &["@leads"], "@boss please decide");
    assert!(p.notice_wakes(&said, "boss"));
    assert!(!p.notice_wakes(&said, "web"), "only the one mentioned");
    // A non-member who is mentioned is not a recipient and not woken.
    let outside = notice("perf", &["@perf"], "@web look");
    assert!(!p.notice_wakes(&outside, "web"));
    // Direct notices wake as before.
    assert!(p.notice_wakes(&notice("perf", &["boss"], "x"), "boss"));
    assert!(!p.notice_wakes(&notice("boss", &["boss"], "x"), "boss"));
    assert!(mentions("hi @perf-a, ok", "perf-a"));
    assert!(!mentions("hi @perf-ab", "perf-a"));
    assert!(!mentions("mail x@perf-a", "perf-a"));
}

#[test]
fn notify_checks_groups_and_read_notices_gives_history() {
    let mut p = project("history");
    assert!(
        p.notify("web-a", &["@perf".into()], "hi").is_err(),
        "not a member"
    );
    assert!(p.notify("web-a", &["@nope".into()], "hi").is_err());
    p.notify("perf-a", &["@perf".into()], "first").unwrap();
    p.notify("perf", &["@perf".into()], "second").unwrap();
    p.notify("perf", &["boss".into()], "private").unwrap();
    // The controller may post to any group.
    p.notify("boss", &["@web".into()], "from boss").unwrap();
    let history = p.group_history("perf-a", "@perf").unwrap();
    assert!(history.contains("first") && history.contains("second"));
    assert!(!history.contains("private") && !history.contains("from boss"));
    assert!(p.group_history("web-a", "perf").is_err());
    assert!(p.group_history("boss", "perf").is_err());
    assert!(p.group_history("perf", "nope").is_err());
    // Through the MCP tools, with and without the group.
    let target = mcp::Target {
        root: p.root.clone(),
        stream: "perf-a".into(),
    };
    assert!(call(&target, "read_notices", json!({"group": "@perf"})).contains("first"));
    assert!(call(&target, "read_notices", json!({})).contains("second"));
    assert!(call(&target, "list_groups", json!({})).contains("`@perf`"));
    let other = mcp::Target {
        root: p.root.clone(),
        stream: "web-a".into(),
    };
    assert!(call(&other, "read_notices", json!({"group": "perf"})).contains("not in"));
}

#[test]
fn group_targets_round_trip_through_notices_md() {
    let mut p = project("roundtrip");
    p.notify(
        "perf",
        &["@perf".into(), "boss".into()],
        "line one\nline two",
    )
    .unwrap();
    let text = fs::read_to_string(p.dir().join("notices.md")).unwrap();
    let parsed = parse_notices(&text);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].to, ["@perf", "boss"]);
    assert_eq!(parsed[0].text, "line one\nline two");
    assert_eq!(p.notices, parsed);
    // Old files, with no `@` targets, parse as before.
    let old = "# Notices\n\n- 2026-01-01 10:00 · web → all: shipped\n- 2026-01-01 10:05 · perf → `web`, boss: api changed\n";
    let old = parse_notices(old);
    assert_eq!(old.len(), 2);
    assert_eq!(old[0].to, ["all"]);
    assert_eq!(old[1].to, ["web", "boss"]);
}

#[test]
fn tools_and_kickoffs_name_groups() {
    let p = project("kickoff");
    let tools = |stream: &str| -> Vec<String> {
        let target = mcp::Target {
            root: p.root.clone(),
            stream: stream.into(),
        };
        let reply = mcp::handle(
            &target,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .unwrap();
        reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    };
    assert!(tools("boss").contains(&"create_group".to_string()));
    assert!(!tools("perf").contains(&"create_group".to_string()));
    assert!(tools("perf").contains(&"list_groups".to_string()));
    let lead = p.kickoff(p.stream("perf").unwrap());
    assert!(lead.contains("Your groups: `@leads`, `@perf`. Post there"));
    let worker = p.kickoff(p.stream("perf-a").unwrap());
    assert!(worker.contains("Your groups: `@perf`."));
    assert!(!p.kickoff(p.stream("boss").unwrap()).contains("Your groups"));
}
