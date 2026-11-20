//! Looks for a newer Atrium a little after start, then once a day.

use std::time::Duration;

use gpui::{AppContext, Context};

use super::Workspace;
use crate::updates;

impl Workspace {
    pub(super) fn start_update_checks(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(20)).await;
            loop {
                let Ok(skip) = this.update(cx, |ws, _| ws.settings.no_update_check) else {
                    return;
                };
                if !skip {
                    let _ = this.update(cx, |ws, cx| ws.check_for_update(false, cx));
                }
                cx.background_executor().timer(Duration::from_secs(24 * 3600)).await;
            }
        })
        .detach();
    }

    /// Looks now; `told` says the outcome even when there is nothing new.
    pub fn check_for_update(&mut self, told: bool, cx: &mut Context<Self>) {
        let job = cx.background_spawn(async move { updates::check() });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |ws, cx| {
                match result {
                    Ok(update) => {
                        if told && update.is_none() {
                            cx.emit(super::Notice::Info(
                                crate::i18n::t("You have the latest Atrium.").into(),
                            ));
                        }
                        ws.update = update;
                    }
                    Err(e) => {
                        log::info!("update check: {e:#}");
                        if told {
                            cx.emit(super::Notice::Info(
                                crate::i18n::t("Could not check for updates.").into(),
                            ));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
