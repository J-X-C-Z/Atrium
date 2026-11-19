use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, StatusCode, header, uri::Authority},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Semaphore;

pub(crate) async fn query(cli: Option<&Path>, args: &[&str]) -> Option<Value> {
    let mut cmd = tokio::process::Command::new(cli?);
    cmd.args(args).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(3), cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() || out.stdout.len() > 4 * 1024 * 1024 {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

fn tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.octets()[0] == 100 && (64..128).contains(&v.octets()[1]),
        IpAddr::V6(v) => v.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

pub(crate) fn addresses(status: &Value) -> impl Iterator<Item = IpAddr> + '_ {
    status["TailscaleIPs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str()?.parse().ok())
        .filter(|ip| tailnet(*ip))
}

#[derive(Clone)]
pub(crate) struct Guard {
    ip: IpAddr,
    port: u16,
    cli: Option<PathBuf>,
    queries: Arc<Semaphore>,
}

impl Guard {
    pub(crate) fn new(ip: IpAddr, port: u16, cli: Option<PathBuf>) -> Self {
        Self {
            ip,
            port,
            cli,
            queries: Arc::new(Semaphore::new(8)),
        }
    }
    async fn allows(&self, peer: SocketAddr, headers: &HeaderMap) -> bool {
        let Some(host) = browser_host(headers, self.port) else {
            return false;
        };
        let numeric = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .ok();
        if self.ip.is_loopback() {
            return peer.ip().is_loopback()
                && (numeric == Some(self.ip) || host.eq_ignore_ascii_case("localhost"));
        }
        if !tailnet(self.ip) || !tailnet(peer.ip()) {
            return false;
        }
        let Ok(_permit) = self.queries.try_acquire() else {
            return false;
        };
        let peer = peer.to_string();
        let whois = ["whois", "--json", &peer];
        let (who, status) = tokio::join!(
            query(self.cli.as_deref(), &whois),
            query(self.cli.as_deref(), &["status", "--json"])
        );
        let (Some(who), Some(status)) = (who, status) else {
            return false;
        };
        let dns = status["Self"]["DNSName"]
            .as_str()
            .unwrap_or("")
            .trim_end_matches('.');
        let named = !dns.is_empty()
            && (dns.eq_ignore_ascii_case(&host)
                || dns
                    .split('.')
                    .next()
                    .is_some_and(|s| s.eq_ignore_ascii_case(&host)));
        addresses(&status).any(|ip| ip == self.ip)
            && (numeric == Some(self.ip) || named)
            && owns_peer(&who, &status)
    }
}

fn owns_peer(who: &Value, status: &Value) -> bool {
    let id = &status["Self"]["UserID"];
    let uid = id
        .as_u64()
        .map(|n| n.to_string())
        .or_else(|| id.as_str().map(str::to_owned));
    let owner = uid.and_then(|id| status["User"][&id]["LoginName"].as_str());
    owner.is_some_and(|name| {
        !name.is_empty() && Some(name) == who["UserProfile"]["LoginName"].as_str()
    })
}

fn browser_host(headers: &HeaderMap, port: u16) -> Option<String> {
    let mut hosts = headers.get_all(header::HOST).iter();
    let host = hosts.next()?.to_str().ok()?;
    let authority: Authority = host.parse().ok()?;
    if hosts.next().is_some() || host.contains('@') || authority.port_u16() != Some(port) {
        return None;
    }
    for origin in headers.get_all(header::ORIGIN) {
        if !origin
            .to_str()
            .ok()?
            .eq_ignore_ascii_case(&format!("http://{host}"))
        {
            return None;
        }
    }
    if headers
        .get_all("sec-fetch-site")
        .iter()
        .any(|v| !matches!(v.as_bytes(), b"same-origin" | b"none"))
    {
        return None;
    }
    Some(authority.host().to_owned())
}

pub(crate) async fn authorize(
    State(guard): State<Guard>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if !guard.allows(peer, req.headers()).await {
        return (StatusCode::FORBIDDEN, Json(json!({"error":"not_allowed"}))).into_response();
    }
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("x-frame-options", "DENY".parse().unwrap());
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_owner_login_and_tailnet_addresses() {
        let status = json!({"Self":{"UserID":42},"User":{"42":{"LoginName":"owner"}},
            "TailscaleIPs":["100.64.0.1","fd7a:115c:a1e0::1","192.168.2.1","::","0.0.0.0"]});
        assert!(owns_peer(
            &json!({"UserProfile":{"LoginName":"owner"}}),
            &status
        ));
        assert!(!owns_peer(
            &json!({"UserProfile":{"LoginName":"someone-else"}}),
            &status
        ));
        assert!(!owns_peer(&json!({}), &json!({})));
        assert_eq!(
            addresses(&status).collect::<Vec<_>>(),
            [
                "100.64.0.1".parse::<IpAddr>().unwrap(),
                "fd7a:115c:a1e0::1".parse().unwrap()
            ]
        );
    }
    #[tokio::test]
    async fn phone_auth_asks_whois_for_the_socket_and_checks_magicdns() {
        use std::fs;
        let dir = std::env::temp_dir().join(format!("atrium-phone-whois-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(if cfg!(windows) {
            "tailscale.cmd"
        } else {
            "tailscale"
        });
        let status = r#"{"Self":{"UserID":"1","DNSName":"desk.example.ts.net."},"User":{"1":{"LoginName":"owner"}},"TailscaleIPs":["100.64.0.1"]}"#;
        let owner = r#"{"UserProfile":{"LoginName":"owner"}}"#;
        let other = r#"{"UserProfile":{"LoginName":"guest"}}"#;
        let script = if cfg!(windows) {
            format!(
                "@echo off\r\nif \"%1\"==\"status\" (echo {status}) else if \"%3\"==\"100.64.0.2:2345\" (echo {owner}) else (echo {other})\r\n"
            )
        } else {
            format!(
                "#!/bin/sh\nif [ \"$1\" = status ]; then echo '{status}'; elif [ \"$3\" = '100.64.0.2:2345' ]; then echo '{owner}'; else echo '{other}'; fi\n"
            )
        };
        fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let guard = Guard::new("100.64.0.1".parse().unwrap(), 7879, Some(path));
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "desk.example.ts.net:7879".parse().unwrap());
        let peer = "100.64.0.2:2345".parse().unwrap();
        assert!(guard.allows(peer, &headers).await);
        assert!(
            !guard
                .allows("100.64.0.3:2345".parse().unwrap(), &headers)
                .await
        );
        headers.insert(header::HOST, "unknown.example:7879".parse().unwrap());
        assert!(!guard.allows(peer, &headers).await);
        fs::remove_dir_all(dir).unwrap();
    }
}
