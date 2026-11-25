//! Atrium's projects, teams and workflows as JSON operations, for the Orca
//! plugin's worker. Built for `wasm32-wasip1` and loaded with `node:wasi`;
//! also an rlib so the ops are tested natively.
//!
//! ABI: `alloc(len) -> ptr`, `dealloc(ptr, len)`, and `call(ptr, len) -> u64`
//! taking `{"op": "...", "args": {...}}` and returning `ptr << 32 | len` of
//! `{"ok": ...}` or `{"err": "..."}`. The caller frees the result with
//! `dealloc`.
//!
//! The core never starts processes or uses the network: what needs one is
//! returned as an effect for the worker to carry out (see [`workflow`] and
//! [`project`]).

pub mod project;
pub mod store;
pub mod team;
pub mod workflow;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// Runs one op on JSON args.
pub fn dispatch(op: &str, args: Value) -> Result<Value, String> {
    match op {
        "core.info" => Ok(json!({
            "version": env!("CARGO_PKG_VERSION"),
            "wasi": cfg!(target_os = "wasi"),
            "projectScheduler": store::scheduler_writes(),
        })),
        "project.init" => project::init(parse(args)?),
        "project.open" => project::open(parse(args)?),
        "project.task.create" => project::task_create(parse(args)?),
        "project.task.update" => project::task_update(parse(args)?),
        "project.task.move" => project::task_move(parse(args)?),
        "project.task.approve" => project::task_approve(parse(args)?),
        "project.notice.add" => project::notice_add(parse(args)?),
        "project.notices.read" => project::notices_read(parse(args)?),
        "project.autopilot.set" => project::autopilot_set(parse(args)?),
        "project.kickoff" => project::kickoff(parse(args)?),
        "project.continuation" => project::continuation(parse(args)?),
        "project.bind" => project::bind(parse(args)?),
        "project.stream.failed" => project::stream_failed(parse(args)?),
        "project.next" => project::next(parse(args)?),
        "team.read" => team::read(parse(args)?),
        "team.write" => team::write(parse(args)?),
        "team.pick" => team::pick(parse(args)?),
        "team.roster" => team::roster(parse(args)?),
        "workflow.templates" => workflow::templates(parse(args)?),
        "workflow.template.parse" => workflow::template_parse(parse(args)?),
        "workflow.template.render" => workflow::template_render(parse(args)?),
        "workflow.template.validate" => workflow::template_validate(parse(args)?),
        "workflow.run.start" => workflow::run_start(parse(args)?),
        "workflow.run.load" => workflow::run_load(parse(args)?),
        "workflow.run.next" => workflow::run_next(parse(args)?),
        "workflow.run.bind" => workflow::run_bind(parse(args)?),
        "workflow.run.report" => workflow::run_report(parse(args)?),
        "workflow.run.check_result" => workflow::run_check_result(parse(args)?),
        "workflow.run.decide" => workflow::run_decide(parse(args)?),
        "workflow.run.retry" => workflow::run_retry(parse(args)?),
        "workflow.run.recover" => workflow::run_recover(parse(args)?),
        "workflow.run.pause" => workflow::run_control(parse(args)?, workflow::Control::Pause),
        "workflow.run.resume" => workflow::run_control(parse(args)?, workflow::Control::Resume),
        "workflow.run.cancel" => workflow::run_control(parse(args)?, workflow::Control::Cancel),
        "workflow.experience" => workflow::experience(parse(args)?),
        _ => Err(format!("unknown op {op}")),
    }
}

fn parse<T: DeserializeOwned>(args: Value) -> Result<T, String> {
    let args = if args.is_null() { json!({}) } else { args };
    serde_json::from_value(args).map_err(|e| format!("bad args: {e}"))
}

/// A request's JSON text in, the `{"ok"}` / `{"err"}` JSON text out.
pub fn handle(input: &[u8]) -> String {
    #[derive(serde::Deserialize)]
    struct Request {
        op: String,
        #[serde(default)]
        args: Value,
    }
    let reply = match serde_json::from_slice::<Request>(input) {
        Ok(request) => match dispatch(&request.op, request.args) {
            Ok(value) => json!({ "ok": value }),
            Err(error) => json!({ "err": error }),
        },
        Err(error) => json!({ "err": format!("bad request: {error}") }),
    };
    reply.to_string()
}

/// Allocates `len` bytes for the caller to write a request into.
#[unsafe(no_mangle)]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr
}

/// Frees a buffer from [`alloc`] or a result of [`call`].
///
/// # Safety
/// `ptr` and `len` must be exactly what `alloc` was given and returned, or
/// what `call` packed into its result, and the buffer must not be used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        // SAFETY: both kinds of buffer have capacity `len.max(1)`.
        drop(unsafe { Vec::from_raw_parts(ptr, 0, len.max(1)) });
    }
}

/// Runs one request and returns the packed pointer and length of its reply.
///
/// # Safety
/// `ptr` must point to `len` readable bytes, such as a buffer from [`alloc`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn call(ptr: *const u8, len: usize) -> u64 {
    // SAFETY: the caller guarantees `len` readable bytes at `ptr`.
    let input = unsafe { std::slice::from_raw_parts(ptr, len) };
    let reply = handle(input).into_bytes();
    let len = reply.len();
    let out = alloc(len);
    // SAFETY: `out` has room for `len.max(1)` bytes and does not overlap `reply`.
    unsafe { std::ptr::copy_nonoverlapping(reply.as_ptr(), out, len) };
    ((out as u64) << 32) | len as u64
}

/// Milliseconds since the epoch, or `now` when the caller gave one.
pub(crate) fn now_ms(now: Option<i64>) -> i64 {
    now.unwrap_or_else(|| jiff::Timestamp::now().as_millisecond())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_answer_ok_or_err() {
        let ok: Value = serde_json::from_str(&handle(br#"{"op":"core.info"}"#)).unwrap();
        assert_eq!(ok["ok"]["wasi"], false);
        let err: Value = serde_json::from_str(&handle(br#"{"op":"nope","args":{}}"#)).unwrap();
        assert_eq!(err["err"], "unknown op nope");
        let bad: Value = serde_json::from_str(&handle(b"not json")).unwrap();
        assert!(bad["err"].as_str().unwrap().starts_with("bad request"));
    }

    #[test]
    fn buffers_are_allocated_and_freed() {
        let ptr = alloc(16);
        assert!(!ptr.is_null());
        unsafe { dealloc(ptr, 16) };
    }
}
