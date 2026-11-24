//! The per-device Atrium node.
//!
//! It runs agents for other devices, exposes their folders, and — when an
//! Atrium embeds it — serves the multi-device overlay and the owner-side
//! conversation API.

pub mod auth;
pub mod config;
pub mod engine;
pub mod fs;
pub mod magpie;
pub mod procs;
pub mod server;
pub mod service;
pub mod sync;

use anyhow::Result;
use std::{collections::HashMap, net::IpAddr, sync::Arc, time::Duration};
use tokio::{sync::watch, task::JoinHandle};

pub use sync::{HostError, HostFuture, SyncHost, SyncState};

/// The command line entry point used by the `atrium-node` binary.
pub async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() == ["config", "path"] {
        println!("{}", config::path().display());
        return Ok(());
    }
    let mut config_path = config::path();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "serve" | "install" | "uninstall" | "status" => {}
            "--config" => {
                index += 1;
                config_path = args
                    .get(index)
                    .ok_or_else(|| anyhow::anyhow!("missing config path"))?
                    .into()
            }
            _ => anyhow::bail!(
                "supported: serve/install/uninstall/status [--config PATH], config path"
            ),
        }
        index += 1;
    }
    if let Some(action) = args
        .first()
        .filter(|value| ["install", "uninstall", "status"].contains(&value.as_str()))
    {
        return service::run(action, &config_path);
    }
    let mut loaded = config::Config::load(&config_path)?;
    let path = config::login_path().await;
    config::apply_fallback(&mut loaded, &path);
    if loaded.engine_db.is_none() {
        loaded.engine_db = Some(config::default_engine_db());
    }
    let config = Arc::new(loaded);
    let state = server::State::new(config.clone(), path.clone()).await?;
    let router = server::router(state.clone());
    let mut listeners: HashMap<IpAddr, (watch::Sender<bool>, JoinHandle<()>)> = HashMap::new();
    let loopback: IpAddr = "127.0.0.1".parse()?;
    let listener = tokio::net::TcpListener::bind((loopback, config.port)).await?;
    println!("listening on {}", listener.local_addr()?);
    let (stop, stop_rx) = watch::channel(false);
    listeners.insert(
        loopback,
        (stop, tokio::spawn(serve(listener, router.clone(), stop_rx))),
    );
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    let mut maintenance = tokio::time::interval(Duration::from_secs(1));
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let exit = async {
        #[cfg(unix)]
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=terminate.recv()=>{}}
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    };
    tokio::pin!(exit);
    loop {
        tokio::select! {
            _ = &mut exit => break,
            _ = maintenance.tick() => state.procs.maintenance().await,
            _ = tick.tick() => {
                let desired = if config.loopback_only {
                    Vec::new()
                } else {
                    auth::addresses(&path).await
                };
                let old: Vec<_> = listeners
                    .keys()
                    .copied()
                    .filter(|ip| *ip != loopback && !desired.contains(ip))
                    .collect();
                for ip in old {
                    if let Some((stop, _)) = listeners.remove(&ip) {
                        let _ = stop.send(true);
                    }
                }
                for ip in desired {
                    if let std::collections::hash_map::Entry::Vacant(entry) = listeners.entry(ip) {
                        match tokio::net::TcpListener::bind((ip, config.port)).await {
                            Ok(listener) => {
                                let (stop, stop_rx) = watch::channel(false);
                                entry.insert((
                                    stop,
                                    tokio::spawn(serve(listener, router.clone(), stop_rx)),
                                ));
                            }
                            Err(error) => eprintln!("bind {ip}: {error}"),
                        }
                    }
                }
            }
        }
    }
    for (stop, _) in listeners.values() {
        let _ = stop.send(true);
    }
    state.procs.shutdown().await;
    for (_, (stop, handle)) in listeners {
        let _ = stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(10), handle).await;
    }
    Ok(())
}

/// A node started inside Atrium, with the overlay and owner-side host.
pub struct EmbeddedNode {
    port: u16,
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    maintenance: JoinHandle<()>,
    state: Arc<server::State>,
}

impl EmbeddedNode {
    pub async fn start(
        config: Arc<config::Config>,
        path: String,
        sync: Arc<SyncState>,
    ) -> Result<Self> {
        let mut config = (*config).clone();
        if config.engine_db.is_none() {
            config.engine_db = Some(config::default_engine_db());
        }
        let config = Arc::new(config);
        let state = server::State::new_with_sync(config.clone(), path.clone(), Some(sync)).await?;
        let router = server::router(state.clone());
        let loopback: IpAddr = "127.0.0.1".parse()?;
        let listener = tokio::net::TcpListener::bind((loopback, config.port)).await?;
        let port = listener.local_addr()?.port();
        let (stop, stop_rx) = watch::channel(false);
        let mut tasks = vec![tokio::spawn(serve(listener, router.clone(), stop_rx))];
        // Tailnet addresses appear and disappear; the desktop restarts the
        // embedded node when the machine joins or leaves one. Binding the
        // current set here keeps startup fast and the code small.
        if !config.loopback_only {
            for ip in auth::addresses(&path).await {
                match tokio::net::TcpListener::bind((ip, port)).await {
                    Ok(listener) => {
                        let stop_rx = stop.subscribe();
                        tasks.push(tokio::spawn(serve(listener, router.clone(), stop_rx)));
                    }
                    Err(error) => log::warn!("embedded node: cannot bind {ip}: {error}"),
                }
            }
        }
        let maintenance_state = state.clone();
        let mut maintenance_stop = stop.subscribe();
        let maintenance = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = maintenance_stop.changed() => break,
                    _ = tick.tick() => maintenance_state.procs.maintenance().await,
                }
            }
        });
        Ok(Self {
            port,
            stop,
            tasks,
            maintenance,
            state,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn local_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        self.state.procs.shutdown().await;
        self.maintenance.abort();
        for task in std::mem::take(&mut self.tasks) {
            let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        }
    }
}

impl Drop for EmbeddedNode {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.maintenance.abort();
    }
}

async fn serve(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    mut stop: watch::Receiver<bool>,
) {
    let result = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        while !*stop.borrow() {
            if stop.changed().await.is_err() {
                break;
            }
        }
    })
    .await;
    if let Err(error) = result {
        eprintln!("listener: {error}");
    }
}
