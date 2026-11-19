#[allow(dead_code)]
#[path="../../../../crates/atrium-app/src/project.rs"]
mod project;
use project::{Project,Workstream};
use project::notice_delivery::{NoticePreparation,NoticeOutcome};
use std::{path::PathBuf,time::Instant};
use serde_json::json;
fn summarize(v:&[u128])->serde_json::Value { let mut s=v.to_vec(); s.sort(); json!({"samples":s.len(),"p50_ns":s[49],"p95_ns":s[94],"total_ns":s.iter().sum::<u128>()}) }
fn main() {
 let base=PathBuf::from(std::env::var_os("NOTICE_COST_FIXTURE_PARENT").expect("set NOTICE_COST_FIXTURE_PARENT to an isolated writable directory")).join(format!("fixture-{}",uuid::Uuid::new_v4()));
 std::fs::create_dir(&base).unwrap();
 let mut prep=vec![];let mut settle=vec![];let mut old=vec![];let mut totals=vec![];
 for mode in ["reservation","cursor_reference"] {
  let root=base.join(mode);std::fs::create_dir(&root).unwrap();
  let mut p=Project::open(&root).unwrap();
  p.file.streams.push(Workstream{id:"lead".into(),thread_id:Some("fixture".into()),backend:"mock".into(),..Default::default()});p.save().unwrap();
  let wall=Instant::now();
  for i in 0..100 {
   p.post("atrium",vec!["lead".into()],&format!("Fixture notice {i:03}: small bounded payload.")).unwrap();
   if mode=="reservation" {
    let t=Instant::now();let NoticePreparation::Ready{ticket,..}=p.prepare_notice_delivery("lead","fixture").unwrap() else {panic!("not ready")};prep.push(t.elapsed().as_nanos());
    let t=Instant::now();assert!(p.settle_notice_delivery(&ticket,NoticeOutcome::Accepted).unwrap());settle.push(t.elapsed().as_nanos());
   } else {let t=Instant::now();p.mark_read("lead");p.save().unwrap();old.push(t.elapsed().as_nanos());}
   assert!(p.unread("lead").is_empty());
  }
  totals.push(json!({"mode":mode,"wall_including_append_ns":wall.elapsed().as_nanos(),"notices_bytes":std::fs::metadata(root.join(".atrium/notices.md")).unwrap().len(),"metadata_bytes":std::fs::metadata(root.join(".atrium/project.json")).unwrap().len()}));
 }
 println!("{}",serde_json::to_string_pretty(&json!({"prepare":summarize(&prep),"settle":summarize(&settle),"legacy_mark_read_save_reference":summarize(&old),"rounds":totals,"raw_ns":{"prepare":prep,"settle":settle,"reference":old}})).unwrap());
 std::fs::remove_dir_all(base).unwrap();
}
