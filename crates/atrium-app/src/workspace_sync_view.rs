//! What the interface asks of multi-device sync: which device a chat lives
//! on, whether it can be reached, who runs a project's scheduler, the
//! pairing code, and pairing with another device by its code.

use std::path::Path;

use atrium_sync::{Lease, PeerClient};
use gpui::Context;

use super::{DeviceStatus, Notice, Workspace};
use crate::i18n::{t, tf};

/// Which chats the list shows, by the device they live on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DeviceFilter {
    #[default]
    All,
    /// Chats whose agent runs on this computer.
    Here,
    /// Chats of one other device, by its device id.
    Device(String),
}

impl Workspace {
    /// The device id a chat's agent runs on, when not this computer.
    pub fn chat_device(&self, thread_id: &str) -> Option<String> {
        if let Some(owner) = self.sync_owner(thread_id)
            && owner != self.settings.install_id
        {
            return self.sync.as_ref()?.devices.get(&owner).cloned();
        }
        let backend = self
            .threads
            .iter()
            .find(|t| t.id == thread_id)?
            .backend
            .clone();
        crate::agents::split_backend(&backend).1.map(str::to_owned)
    }

    /// Other devices with chats in the list, in a steady order.
    pub fn chat_devices(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .threads
            .iter()
            .filter_map(|t| self.chat_device(&t.id))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// What to call `device` on screen: the name the user gave it, else its id.
    pub fn device_label(&self, device: &str) -> String {
        self.settings.device_label(device)
    }

    /// A backend's name with the device's display name: `Claude Code · office`.
    pub fn backend_label(&self, backend: &str) -> String {
        crate::agents::backend_label_with(backend, |device| self.device_label(device))
    }

    /// Gives `device` a display name; an empty one clears it.
    pub fn set_device_alias(&mut self, device: &str, name: &str, cx: &mut Context<Self>) {
        self.settings.set_device_alias(device, name);
        self.settings.save();
        cx.notify();
    }

    /// Worth talking to: online, or reconnecting after a blip.
    pub fn device_online(&self, device: &str) -> bool {
        self.device(device).is_some_and(|d| d.status.is_reachable())
    }

    /// Whether the chat list shows `thread_id` under the current filter.
    pub fn chat_shown(&self, thread_id: &str) -> bool {
        match &self.device_filter {
            DeviceFilter::All => true,
            DeviceFilter::Here => self.chat_device(thread_id).is_none(),
            DeviceFilter::Device(d) => self.chat_device(thread_id).as_deref() == Some(d),
        }
    }

    pub fn set_device_filter(&mut self, filter: DeviceFilter, cx: &mut Context<Self>) {
        self.device_filter = filter;
        cx.notify();
    }

    /// The shown chat lives on another device that cannot be reached now:
    /// it shows what was last synced, and cannot be written to.
    pub fn offline_owner(&self) -> Option<String> {
        let id = self.active_thread_id()?;
        let device = self.chat_device(id)?;
        (!self.device_online(&device)).then_some(device)
    }

    /// The shown chat lives on a device that missed a poll or whose link is
    /// getting back: not offline yet.
    pub fn reconnecting_owner(&self) -> Option<String> {
        let id = self.active_thread_id()?;
        let device = self.chat_device(id)?;
        (self.device_state(&device) == DeviceStatus::Reconnecting).then_some(device)
    }

    /// The other device that runs `root`'s scheduler, while its lease holds.
    pub fn scheduler_elsewhere(&self, root: &Path) -> Option<String> {
        let sync = self.sync.as_ref()?;
        let id = sync.project_ids.get(&super::canonical_path(root))?;
        let lease: Lease = serde_json::from_value(
            sync.state
                .store()
                .value(&format!("project/{id}/scheduler"))?
                .clone(),
        )
        .ok()?;
        if lease.device == self.settings.install_id || lease.until_ms < atrium_node::sync::now_ms()
        {
            return None;
        }
        Some(
            sync.devices
                .get(&lease.device)
                .cloned()
                .unwrap_or(lease.device),
        )
    }

    /// Whether this computer shares its chats with the user's other devices.
    pub fn sharing(&self) -> bool {
        self.settings.sync_enabled()
    }

    pub fn set_sharing(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.sync_enabled = Some(on);
        self.settings.save();
        cx.emit(Notice::Info(
            t("Sharing changes take effect the next time Atrium starts.").into(),
        ));
        cx.notify();
    }

    pub fn set_device_name(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.trim();
        if name.is_empty() || name == self.settings.device_name {
            return;
        }
        self.settings.device_name = name.to_string();
        self.settings.save();
        cx.emit(Notice::Info(
            t("Other devices see the new name the next time Atrium starts.").into(),
        ));
        cx.notify();
    }

    /// A one-time code another device types to pair with this one.
    pub fn pairing_code(&mut self, cx: &mut Context<Self>) {
        let Some(sync) = &self.sync else {
            cx.emit(Notice::Error(
                t("Sharing is off on this computer, so it cannot pair.").into(),
            ));
            return;
        };
        let code = sync.state.start_pairing(atrium_node::sync::now_ms());
        self.pairing = Some((code, std::time::Instant::now()));
        cx.notify();
    }

    /// The code shown here, while it is still good (five minutes).
    pub fn shown_pairing_code(&self) -> Option<(String, u64)> {
        let (code, at) = self.pairing.as_ref()?;
        let left = 300u64.checked_sub(at.elapsed().as_secs())?;
        Some((code.clone(), left))
    }

    /// Pairs with the device at `address` using the code it shows, then
    /// keeps its token and adds it to the devices.
    pub fn pair_with(&mut self, address: String, code: String, cx: &mut Context<Self>) {
        let address = address.trim().trim_end_matches('/').to_string();
        let code = code.trim().to_string();
        if address.is_empty() || code.is_empty() {
            return;
        }
        let base = if address.starts_with("http") {
            address.clone()
        } else {
            format!("http://{address}")
        };
        cx.spawn(async move |this, cx| {
            let joined = cx
                .background_executor()
                .spawn(async move { PeerClient::new(base, None).pair_join(&code) })
                .await;
            let _ = this.update(cx, |ws, cx| match joined {
                Ok(token) => {
                    ws.settings.paired.insert(address.clone(), token);
                    if !ws.settings.nodes.contains(&address) {
                        ws.settings.nodes.push(address.clone());
                    }
                    ws.settings.save();
                    cx.emit(Notice::Success(tf(
                        "Paired with {address}.",
                        &[("address", &address)],
                    )));
                    ws.discover(cx);
                }
                Err(error) => cx.emit(Notice::Error(pairing_error(&error.to_string()))),
            });
        })
        .detach();
    }
}

fn pairing_error(raw: &str) -> String {
    if raw.contains("403") {
        t("That code is wrong or has expired. Make a new one on the other computer.").into()
    } else if raw.contains("Connect") || raw.contains("connect") {
        t("That computer cannot be reached. Check the address and that Atrium runs there.").into()
    } else {
        tf("Could not pair: {why}", &[("why", &raw)])
    }
}

/// A move's failure in words, from the error the other device gave.
pub fn move_error_text(raw: &str, device: &str) -> String {
    if raw.contains("no session file") {
        tf(
            "{device} has no saved session for this conversation, so it cannot be moved. It may belong to another agent, or the session was cleaned up.",
            &[("device", &device)],
        )
    } else if raw.contains("Connect") || raw.contains("connect") || raw.contains("timed out") {
        tf(
            "{device} cannot be reached, so the conversation stays there.",
            &[("device", &device)],
        )
    } else if raw.contains("409") {
        tf(
            "{device} is busy with this conversation. Try again when it stops.",
            &[("device", &device)],
        )
    } else {
        tf(
            "Could not move the conversation from {device}.",
            &[("device", &device)],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_errors_read_as_words() {
        let raw = r#"sync peer returned 409: {"error":"agent_unavailable","message":"Codex has no session file for this conversation"}"#;
        assert!(move_error_text(raw, "studio").contains("no saved session"));
        assert!(!move_error_text(raw, "studio").contains('{'));
    }
}
