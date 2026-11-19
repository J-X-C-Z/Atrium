//! Read-only published snapshots and a bounded request/reply channel to the desktop.
mod auth;
mod http;
pub mod push;
pub mod snapshot;

use serde_json::Value;
use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot, watch},
};

#[derive(Default, PartialEq)]
pub struct ProjectSnapshot {
    pub summary: Value,
    pub detail: Value,
    pub threads: BTreeMap<String, Arc<Vec<Value>>>,
    /// Compact routing information for pending requests.
    pub pending: Vec<Value>,
}

pub type Snapshot = Vec<ProjectSnapshot>;

#[derive(Debug)]
pub enum Action {
    Approval {
        thread: String,
        request_id: Value,
        decision: Decision,
    },
    Question {
        thread: String,
        request_id: Value,
        answers: BTreeMap<String, Vec<String>>,
    },
    Notice {
        project: usize,
        root: String,
        to: Vec<String>,
        text: String,
    },
    Message {
        thread: String,
        text: String,
    },
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Accept,
    AcceptForSession,
    Decline,
}

#[derive(Clone, Copy, Debug)]
pub enum Error {
    Missing,
    Stale,
    Invalid,
    Unavailable,
    WriteFailed,
}
pub struct Command {
    pub action: Action,
    pub reply: oneshot::Sender<Result<(), Error>>,
}

#[derive(Clone)]
struct State {
    snapshots: watch::Receiver<Arc<Snapshot>>,
    commands: mpsc::Sender<Command>,
}

/// Dropping the handle closes listeners, existing event streams and the command channel.
pub struct Server {
    pub snapshots: watch::Sender<Arc<Snapshot>>,
    _stop: oneshot::Sender<()>,
    pushes: mpsc::Sender<push::Notification>,
}

pub type Commands = mpsc::Receiver<Command>;

impl Server {
    pub fn notify(&self, notification: push::Notification) {
        if self.pushes.try_send(notification).is_err() {
            log::warn!("phone notification queue unavailable");
        }
    }
    pub fn start() -> std::io::Result<(Self, Commands)> {
        Self::spawn(|state| listen(state, 7879))
    }

    #[cfg(feature = "test-support")]
    pub fn loopback_for_test(listener: std::net::TcpListener) -> std::io::Result<(Self, Commands)> {
        let addr = listener.local_addr()?;
        assert!(addr.ip().is_loopback());
        listener.set_nonblocking(true)?;
        Self::spawn(move |state| async move {
            let router = http::router(state, auth::Guard::new(addr.ip(), addr.port(), None));
            let listener = TcpListener::from_std(listener).unwrap();
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        })
    }

    fn spawn<F: std::future::Future<Output = ()>>(
        serve: impl FnOnce(State) -> F + Send + 'static,
    ) -> std::io::Result<(Self, Commands)> {
        let (snapshots, read) = watch::channel(Arc::default());
        let (commands, receive) = mpsc::channel(32);
        let (pushes, jobs) = mpsc::channel(16);
        let (stop, stopped) = oneshot::channel();
        std::thread::Builder::new()
            .name("atrium-phone".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                match runtime {
                    Ok(rt) => rt.block_on(async {
                        tokio::spawn(push::deliver(jobs));
                        tokio::select! {
                            _ = stopped => (),
                            _ = serve(State { snapshots: read, commands }) => (),
                        }
                    }),
                    Err(e) => log::error!("phone server runtime: {e}"),
                }
            })?;
        Ok((
            Self {
                snapshots,
                _stop: stop,
                pushes,
            },
            receive,
        ))
    }
}

async fn listen(state: State, port: u16) {
    let local: IpAddr = Ipv4Addr::LOCALHOST.into();
    let cli = tokio::task::spawn_blocking(agent_rpc::node::tailscale_binary)
        .await
        .unwrap_or_default();
    let mut listeners = BTreeMap::<IpAddr, tokio::task::JoinHandle<()>>::new();
    loop {
        let mut ips = vec![local];
        if let Some(status) = auth::query(cli.as_deref(), &["status", "--json"]).await {
            ips.extend(auth::addresses(&status));
        }
        listeners.retain(|ip, task| {
            if !ips.contains(ip) {
                task.abort();
                return false;
            }
            !task.is_finished()
        });
        for ip in ips {
            if listeners.contains_key(&ip) {
                continue;
            }
            match TcpListener::bind((ip, port)).await {
                Ok(listener) => {
                    let router =
                        http::router(state.clone(), auth::Guard::new(ip, port, cli.clone()));
                    listeners.insert(
                        ip,
                        tokio::spawn(async move {
                            if let Err(e) = axum::serve(
                                listener,
                                router
                                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
                            )
                            .await
                            {
                                log::warn!("phone listener {ip}: {e}");
                            }
                        }),
                    );
                }
                Err(e) => log::warn!("phone listener {ip}:{port}: {e}"),
            }
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

#[cfg(test)]
mod tests;
