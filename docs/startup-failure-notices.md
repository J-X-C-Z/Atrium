# Startup failure notices after restart

A failed start with a persisted workstream now leaves a scheduler receipt
in its direct parent notice. Its identity is the parent, task key and
workstream ID/creation time; a changed error string does not create a second
notice for that same failure. The notice is durable before the task is
marked Failed and the unbound child record removed. If the final metadata
save fails, startup recovery can remove that same unbound record on retry.
Bound sessions are left alone. A reopened task avoids child IDs already recorded
in its startup-failure receipts, so two attempts in one second remain distinct.

The normal scheduler scans unread receipts addressed directly to an existing
parent. It can requeue a startup-failure wake even after the worker record and
task file are gone. After the parent's read cursor is saved, restarting no
longer requeues that receipt. Ordinary notices and legacy unmarked failure
prose retain their existing semantics.

This slice deliberately does not assign a persistent lifecycle identity to an
orphan Running task that never had a workstream. Existing orphan failure and
notification handling is unchanged; if the app crashes after the old-format
notice is written and its orphan is marked Failed, a lead may need to read it
manually. Filesystem timestamps cannot distinguish replay from a new claim
under all conditions. This is not a cross-process exactly-once guarantee.
It also does not change when a notice is marked read relative to RPC acceptance.
