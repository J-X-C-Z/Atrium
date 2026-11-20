//! `atrium://` links, from a notification, the phone, a launcher such as
//! Raycast or Alfred, or a macOS Shortcut bound to a key:
//!
//! - `atrium://new?prompt=…&folder=…&send=1`: a new chat, its message
//!   written (and sent with `send=1`), in `folder` when given;
//! - `atrium://project/<ix>/stream/<id>`: a project's workstream;
//! - `atrium://chat/<id>`: a conversation.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    New {
        prompt: String,
        folder: Option<PathBuf>,
        send: bool,
    },
    Stream {
        project: usize,
        stream: String,
    },
    Chat(String),
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()).ok_or(()) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse(url: &str) -> Option<Link> {
    let rest = url.trim().strip_prefix("atrium://")?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let parts: Vec<String> = path.split('/').filter(|p| !p.is_empty()).map(decode).collect();
    let param = |name: &str| {
        query
            .split('&')
            .filter_map(|kv| kv.split_once('=').or(Some((kv, ""))))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| decode(v))
    };
    match parts.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["new"] => Some(Link::New {
            prompt: param("prompt").unwrap_or_default(),
            folder: param("folder").filter(|f| !f.is_empty()).map(PathBuf::from),
            send: matches!(param("send").as_deref(), Some("1" | "true" | "yes")),
        }),
        ["project", ix, "stream", id] => Some(Link::Stream {
            project: ix.parse().ok()?,
            stream: id.to_string(),
        }),
        ["chat", id] => Some(Link::Chat(id.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_parse() {
        assert_eq!(
            parse("atrium://new?prompt=%E4%BF%AE%E4%B8%80%E4%B8%8B+bug&folder=%2Fcode%2Fapp&send=1"),
            Some(Link::New {
                prompt: "修一下 bug".into(),
                folder: Some(PathBuf::from("/code/app")),
                send: true
            })
        );
        assert_eq!(
            parse("atrium://new"),
            Some(Link::New { prompt: String::new(), folder: None, send: false })
        );
        assert_eq!(
            parse("atrium://project/2/stream/frontend-w1"),
            Some(Link::Stream { project: 2, stream: "frontend-w1".into() })
        );
        assert_eq!(parse("atrium://chat/thread-9"), Some(Link::Chat("thread-9".into())));
        assert_eq!(parse("https://example.com"), None);
        assert_eq!(parse("atrium://project/x/stream/a"), None);
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%E4中"), "\u{FFFD}中".replace('\u{FFFD}', &String::from_utf8_lossy(&[0xE4])));
    }
}
