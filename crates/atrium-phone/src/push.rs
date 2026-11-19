//! Publish to a user-selected ntfy topic. Only names and the screen link cross this boundary.
use std::time::Duration;
use url::Url;

pub struct Notification {
    pub topic: String,
    pub title: &'static str,
    pub project: String,
    pub stream: String,
    pub ix: usize,
    pub id: String,
}

pub(crate) async fn deliver(mut jobs: tokio::sync::mpsc::Receiver<Notification>) {
    while let Some(n) = jobs.recv().await {
        if !matches!(
            tokio::task::spawn_blocking(move || publish(
                &n.topic, n.title, &n.project, &n.stream, n.ix, &n.id
            ))
            .await,
            Ok(Ok(()))
        ) {
            log::warn!("phone notification delivery failed");
        }
    }
}

fn publish(
    topic: &str,
    title: &'static str,
    project: &str,
    stream: &str,
    ix: usize,
    id: &str,
) -> Result<(), &'static str> {
    let topic = topic.trim();
    let url = if !topic.is_empty()
        && topic
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        Url::parse(&format!("https://ntfy.sh/{topic}"))
    } else {
        Url::parse(topic)
    }
    .map_err(|_| "invalid topic")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || url.path().trim_matches('/').is_empty()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("invalid topic");
    }
    let mut click = Url::parse("atrium://project").unwrap();
    click
        .path_segments_mut()
        .unwrap()
        .extend([&ix.to_string(), "stream", id]);
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(5))
        .build()
        .post(url.as_str())
        .set("Title", title)
        .set("Click", click.as_str())
        .send_string(&format!("{project} · {stream}"))
        .map_err(|_| "publish failed")
        .and_then(|r| {
            if (200..300).contains(&r.status()) {
                Ok(())
            } else {
                Err("publish rejected")
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, http::HeaderMap, routing::post};
    #[tokio::test]
    async fn phone_push_local_topic_receives_only_names_and_deep_link() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/my/topic", listener.local_addr().unwrap());
        let router = Router::new().route(
            "/my/topic",
            post(|headers: HeaderMap, body: String| async move {
                assert_eq!(headers["title"], "Approval requested");
                assert_eq!(headers["click"], "atrium://project/3/stream/lead%20one");
                assert_eq!(body, "Project · Lead");
            }),
        );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        tokio::task::spawn_blocking(move || {
            publish(&url, "Approval requested", "Project", "Lead", 3, "lead one")
        })
        .await
        .unwrap()
        .unwrap();
    }
}
