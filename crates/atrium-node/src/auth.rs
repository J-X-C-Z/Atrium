use crate::{config::Config, sync::SyncState};
use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub struct Auth {
    pub config: Arc<Config>,
    pub path: String,
    pairing: Option<Arc<SyncState>>,
    cache: Mutex<HashMap<SocketAddr, (Instant, bool)>>,
}
impl Auth {
    pub fn new(config: Arc<Config>, path: String, pairing: Option<Arc<SyncState>>) -> Self {
        Self {
            config,
            path,
            pairing,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

/// The Tailscale login that owns this device, when it can be read.
pub async fn owner(path: &str) -> String {
    let Some(status) = ts(path, &["status", "--json"]).await else {
        return String::new();
    };
    let uid = status["Self"]["UserID"]
        .as_u64()
        .map(|value| value.to_string())
        .or_else(|| status["Self"]["UserID"].as_str().map(str::to_owned))
        .unwrap_or_default();
    status["User"][&uid]["LoginName"]
        .as_str()
        .unwrap_or("")
        .to_string()
}
pub fn tailnet_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let a = v.octets();
            a[0] == 100 && (64..128).contains(&a[1])
        }
        IpAddr::V6(v) => v.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}
pub async fn ts(path: &str, args: &[&str]) -> Option<Value> {
    let binary = crate::config::find_binary("tailscale", path)?;
    let mut cmd = tokio::process::Command::new(binary);
    cmd.args(args).env("PATH", path).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let out = tokio::time::timeout(Duration::from_secs(3), cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() || out.stdout.len() > 4 * 1024 * 1024 {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}
pub async fn addresses(path: &str) -> Vec<IpAddr> {
    ts(path, &["status", "--json"])
        .await
        .and_then(|v| v["TailscaleIPs"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str()?.parse().ok())
        .filter(|ip| tailnet_ip(*ip))
        .collect()
}
fn loopback_host(host: &str) -> bool {
    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    let h = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost") || h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= (a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0)) as usize;
    }
    diff == 0
}
fn allowed_identity(who: &Value, status: &Value, c: &Config) -> bool {
    let login = who["UserProfile"]["LoginName"].as_str().unwrap_or("");
    let uid = status["Self"]["UserID"]
        .as_u64()
        .map(|v| v.to_string())
        .or_else(|| status["Self"]["UserID"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let owner = status["User"][&uid]["LoginName"].as_str().unwrap_or("");
    (!login.is_empty()
        && ((!owner.is_empty() && login == owner) || c.allow_users.iter().any(|u| u == login)))
        || who["Node"]["Tags"].as_array().is_some_and(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .any(|t| c.allow_tags.iter().any(|a| a == t))
        })
}
pub async fn middleware(
    State(auth): State<Arc<Auth>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let join = req.uri().path() == "/v1/sync/pair/join";
    let got = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let bearer = got.strip_prefix("Bearer ").unwrap_or("");
    let config_token = !auth.config.token.is_empty()
        && constant_eq(
            got.as_bytes(),
            format!("Bearer {}", auth.config.token).as_bytes(),
        );
    let paired = !bearer.is_empty()
        && auth
            .pairing
            .as_ref()
            .is_some_and(|sync| sync.accepts_token(bearer));
    if !join {
        if !bearer.is_empty() && !config_token && !paired {
            return error(StatusCode::UNAUTHORIZED, "unauthorized");
        }
        if !auth.config.token.is_empty() && !config_token && !paired {
            return error(StatusCode::UNAUTHORIZED, "unauthorized");
        }
    }
    // Browser pages must not use a local user's ambient loopback access.
    if req.headers().contains_key("origin") {
        return error(StatusCode::FORBIDDEN, "not_allowed");
    }
    // The pairing code is the credential for this one route. It must be
    // reachable by a device that is not yet one of the owner's devices.
    if join {
        return next.run(req).await;
    }
    if peer.ip().is_loopback() {
        let host = req
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !loopback_host(host) {
            return error(StatusCode::FORBIDDEN, "not_allowed");
        }
        return next.run(req).await;
    }
    if !tailnet_ip(peer.ip()) {
        return error(StatusCode::FORBIDDEN, "not_allowed");
    }
    let cached = {
        let cache = auth.cache.lock().await;
        cache
            .get(&peer)
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(60))
            .map(|(_, yes)| *yes)
    };
    if let Some(yes) = cached {
        return if yes || paired {
            next.run(req).await
        } else {
            error(StatusCode::FORBIDDEN, "not_allowed")
        };
    }
    let yes = match (
        ts(&auth.path, &["whois", "--json", &peer.to_string()]).await,
        ts(&auth.path, &["status", "--json"]).await,
    ) {
        (Some(w), Some(s)) => allowed_identity(&w, &s, &auth.config),
        _ => false,
    };
    {
        let mut cache = auth.cache.lock().await;
        cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(60));
        if cache.len() < 1024 {
            cache.insert(peer, (Instant::now(), yes));
        }
    }
    if yes || paired {
        next.run(req).await
    } else {
        error(StatusCode::FORBIDDEN, "not_allowed")
    }
}
fn error(s: StatusCode, code: &str) -> Response {
    (
        s,
        Json(json!({"error":code,"message":"connection is not authorized"})),
    )
        .into_response()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_and_identity() {
        assert!(loopback_host("localhost:7381"));
        assert!(loopback_host("127.0.0.1:7381"));
        assert!(loopback_host("[::1]:7381"));
        assert!(!loopback_host("untrusted.example:7381"));
        assert!(constant_eq(b"abc", b"abc"));
        assert!(!constant_eq(b"abc", b"abcd"));
        let c = Config::default();
        let s = json!({"Self":{"UserID":1},"User":{"1":{"LoginName":"me"}}});
        assert!(allowed_identity(
            &json!({"UserProfile":{"LoginName":"me"}}),
            &s,
            &c
        ));
        assert!(!allowed_identity(&json!({}), &s, &c));
    }
    #[test]
    fn network_boundaries() {
        assert!(tailnet_ip("100.64.0.1".parse().unwrap()));
        assert!(!tailnet_ip("100.128.0.1".parse().unwrap()));
        assert!(!tailnet_ip("192.168.1.2".parse().unwrap()));
    }
}
