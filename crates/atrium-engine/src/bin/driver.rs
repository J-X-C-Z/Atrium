//! Scenario driver for `tools/t3-diff`.
//!
//! Reads one JSON scenario from stdin, drives `atrium-engine` the way the T3
//! scenario drives T3, and writes the normalized event stream as JSON lines to
//! stdout. tools/t3-diff diffs the two streams.

fn main() {
    eprintln!(
        "atrium-engine-driver: no scenarios wired yet; tools/t3-diff owns the harness"
    );
    std::process::exit(2);
}
