//! Optional, bounded local discovery. Never follow a redirect off this device.
use serde_json::{Value, json};
use std::{io::Read, path::Path, time::Duration};
const LIMIT: u64 = 64 * 1024;
fn port(path: &Path) -> Option<u16> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(3425),
        Err(_) => return None,
    };
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > LIMIT as usize {
        return None;
    }
    let settings: Value = serde_json::from_slice(&bytes).ok()?;
    match settings.get("port") {
        None | Some(Value::Null) => Some(3425),
        Some(v) => u16::try_from(v.as_u64()?).ok().filter(|p| *p != 0),
    }
}
fn probe(port: u16) -> Option<Value> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(2))
        .redirects(0)
        .build();
    let response = agent
        .get(&format!("http://127.0.0.1:{port}/api/hello"))
        .call()
        .ok()?;
    if response.status() != 200 {
        return None;
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > LIMIT as usize {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let version = value.get("version")?.as_str()?;
    if version.is_empty() || version.len() > 512 {
        return None;
    }
    Some(json!({"version":version,"port":port}))
}
pub async fn discover() -> Option<Value> {
    let path = dirs::home_dir()?.join(".config/magpie/settings.json");
    tokio::task::spawn_blocking(move || probe(port(&path)?))
        .await
        .ok()
        .flatten()
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn settings_fail_closed_and_validate_port() {
        let root = std::env::temp_dir().join(format!("atrium-magpie-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("settings.json");
        assert_eq!(port(&path), Some(3425));
        for (text, expected) in [
            ("{}", Some(3425)),
            ("{\"port\":4000}", Some(4000)),
            ("{\"port\":0}", None),
            ("{\"port\":65536}", None),
            ("{", None),
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(port(&path), expected);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn bounded_local_probe_and_redirect_refusal() {
        for (status, body, expected) in [
            ("200 OK", "{\"version\":\"1.2.3\"}", true),
            ("200 OK", "{}", false),
            ("302 Found", "", false),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let task = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut buffer = [0; 1024];
                let _ = socket.read(&mut buffer);
                write!(socket,"HTTP/1.1 {status}\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/no-follow\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            });
            assert_eq!(probe(port).is_some(), expected);
            task.join().unwrap();
        }
    }
}
