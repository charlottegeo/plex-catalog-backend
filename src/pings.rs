use crate::models::PingsBody;
use reqwest::Client;
use std::sync::OnceLock;

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

fn client() -> &'static Client {
    HTTP_CLIENT.get_or_init(|| Client::new())
}

/// Sends a ping notification to PINGS_URL
pub async fn send_ping(
    target_username: impl Into<String>,
    body: impl Into<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = match std::env::var("PINGS_URL") {
        Ok(u) if !u.trim().is_empty() => u,
        _ => {
            tracing::warn!("PINGS_URL is not set; skipping ping notification");
            return Ok(());
        }
    };

    let payload = PingsBody {
        username: target_username.into(),
        body: body.into(),
    };

    let resp = client().post(&url).json(&payload).send().await?;
    if !resp.status().is_success() {
        tracing::warn!(
            "Ping request failed: {} - {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        );
    }
    Ok(())
}

/// Details for a new request, used to notify server owners.
pub struct RequestPingDetails<'a> {
    pub username: &'a str,
    pub title: &'a str,
    pub year: Option<i32>,
    pub item_type: &'a str,
    pub requested_seasons: Option<&'a [i32]>,
    pub requested_resolution: Option<&'a str>,
    pub is_upgrade: bool,
}

/// Notify server owners that a user requested new media.
pub async fn send_request_ping(
    details: RequestPingDetails<'_>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let year_str = details
        .year
        .map(|y| format!(" ({})", y))
        .unwrap_or_default();

    let mut parts = vec![format!(
        "{} requested {}{}.",
        details.username, details.title, year_str
    )];

    if details.item_type == "show" {
        if let Some(seasons) = details.requested_seasons {
            if !seasons.is_empty() {
                let s: Vec<String> = seasons.iter().map(|n| n.to_string()).collect();
                parts.push(format!("Seasons: {}.", s.join(", ")));
            }
        }
    }

    if details.is_upgrade {
        parts.push(format!(
            "User is requesting a different resolution for an existing {}.",
            details.item_type
        ));
    } else if let Some(res) = details.requested_resolution {
        if !res.is_empty() {
            parts.push(format!("Preferred resolution: {}.", res));
        }
    }

    let body = parts.join(" ");
    send_ping("server_owners", body).await
}

/// Notify a user that their request has been fulfilled.
pub async fn send_fulfilled_ping(
    username: impl Into<String>,
    title: impl Into<String>,
    resolution: impl AsRef<str>,
    server_names: &[String],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let resolution_str = resolution.as_ref();
    let server_list = if server_names.is_empty() {
        "unknown servers".to_string()
    } else {
        server_names.join(", ")
    };

    let body = format!(
        "Your request for {} was fulfilled with resolution {} and is now available on: {}.",
        title.into(),
        resolution_str,
        server_list
    );

    send_ping(username, body).await
}

/// Notify a user that their unfulfilled request was removed after 30 days.
pub async fn send_stale_ping(
    username: impl Into<String>,
    title: impl Into<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let body = format!(
        "Your request for {} was unfulfilled for 30 days and has been removed.",
        title.into()
    );
    send_ping(username, body).await
}
