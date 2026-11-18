mod auth;
mod config;
mod fs;
mod magpie;
mod procs;
mod server;
mod service;
use anyhow::Result;
use std::{collections::HashMap, net::IpAddr, sync::Arc, time::Duration};
use tokio::sync::watch;
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() == ["config", "path"] {
        println!("{}", config::path().display());
        return Ok(());
    }
    let mut config_path = config::path();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "serve" | "install" | "uninstall" | "status" => {}
            "--config" => {
                i += 1;
                config_path = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("missing config path"))?
                    .into()
            }
            _ => anyhow::bail!(
                "supported: serve/install/uninstall/status [--config PATH], config path"
            ),
        };
        i += 1;
    }
    if let Some(action) = args
        .first()
        .filter(|s| ["install", "uninstall", "status"].contains(&s.as_str()))
    {
        return service::run(action, &config_path);
    }
    let mut loaded = config::Config::load(&config_path)?;
    let path = config::login_path().await;
    config::apply_fallback(&mut loaded, &path);
    let config = Arc::new(loaded);
    let state = server::State::new(config.clone(), path.clone()).await?;
    let router = server::router(state.clone());
    let mut listeners: HashMap<IpAddr, (watch::Sender<bool>, tokio::task::JoinHandle<()>)> =
        HashMap::new();
    let loopback: IpAddr = "127.0.0.1".parse()?;
    let listener = tokio::net::TcpListener::bind((loopback, config.port)).await?;
    println!("listening on {}", listener.local_addr()?);
    let (stop, stop_rx) = watch::channel(false);
    listeners.insert(
        loopback,
        (stop, tokio::spawn(serve(listener, router.clone(), stop_rx))),
    );
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    let mut maint = tokio::time::interval(Duration::from_secs(1));
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let exit = async {
        #[cfg(unix)]
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    };
    tokio::pin!(exit);
    loop {
        tokio::select! {
        _=&mut exit=>break,
        _=maint.tick()=>{state.procs.maintenance().await;},
        _=tick.tick()=>{let desired=if config.loopback_only {Vec::new()}else{auth::addresses(&path).await};let old:Vec<_>=listeners.keys().copied().filter(|p|*p!=loopback&&!desired.contains(p)).collect();for p in old{if let Some((s,_))=listeners.remove(&p){let _=s.send(true);}}for ip in desired{if let std::collections::hash_map::Entry::Vacant(e) = listeners.entry(ip) {match tokio::net::TcpListener::bind((ip,config.port)).await{Ok(l)=>{let(s,r)=watch::channel(false);e.insert((s,tokio::spawn(serve(l,router.clone(),r))));},Err(e)=>eprintln!("bind {ip}: {e}")}}}}
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
    if let Err(e) = result {
        eprintln!("listener: {e}");
    }
}
