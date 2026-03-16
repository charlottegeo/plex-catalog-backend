use reqwest::{
    header::{HeaderMap, HeaderValue},
    Client,
};
use serde_json::json;

#[derive(Clone)]
pub struct PingClient {
    client: Client,
    request_route: String,
    status_route: String,
}

impl PingClient {
    pub fn new(
        token: String,
        request_route: String,
        status_route: String,
    ) -> Result<PingClient, Box<dyn std::error::Error>> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", token))?,
        );

        let client = Client::builder().default_headers(headers).build()?;

        Ok(PingClient {
            client,
            request_route,
            status_route,
        })
    }

    /// Notify a server owner that a user requested new media.
    pub async fn send_request_ping(
        &self,
        to: &str,
        requester: &str,
        title: &str,
        year: Option<i32>,
        item_type: &str,
        requested_seasons: Option<&[i32]>,
        requested_resolution: Option<&str>,
        is_upgrade: bool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let year_str = year.map(|y| format!(" ({})", y)).unwrap_or_default();
        let mut parts = vec![format!("{} requested {}{}.", requester, title, year_str)];

        if item_type == "show" {
            if let Some(seasons) = requested_seasons {
                if !seasons.is_empty() {
                    let s: Vec<String> = seasons.iter().map(|n| n.to_string()).collect();
                    parts.push(format!("Seasons: {}.", s.join(", ")));
                }
            }
        }

        if is_upgrade {
            parts.push(format!(
                "User is requesting a different resolution for an existing {}.",
                item_type
            ));
        } else if let Some(res) = requested_resolution {
            if !res.is_empty() {
                parts.push(format!("Preferred resolution: {}.", res));
            }
        }

        self.client
            .post(format!(
                "https://pings.csh.rit.edu/service/route/{}/ping",
                self.request_route
            ))
            .json(&json!({
                "username": to,
                "body": parts.join(" ")
            }))
            .send()
            .await?;
        Ok(())
    }

    /// Notify a user that their request has been fulfilled.
    pub async fn send_fulfilled_ping(
        &self,
        to: &str,
        title: &str,
        resolution: &str,
        server_names: &[String],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let server_list = if server_names.is_empty() {
            "unknown servers".to_string()
        } else {
            server_names.join(", ")
        };

        let body = format!(
            "Your request for {} was fulfilled with resolution {} and is now available on: {}.",
            title, resolution, server_list
        );

        self.client
            .post(format!(
                "https://pings.csh.rit.edu/service/route/{}/ping",
                self.status_route
            ))
            .json(&json!({
                "username": to,
                "body": body
            }))
            .send()
            .await?;
        Ok(())
    }

    /// Notify a user that their unfulfilled request was removed after 30 days.
    pub async fn send_stale_ping(
        &self,
        to: &str,
        title: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let body = format!(
            "Your request for {} was unfulfilled for 30 days and has been removed.",
            title
        );

        self.client
            .post(format!(
                "https://pings.csh.rit.edu/service/route/{}/ping",
                self.status_route
            ))
            .json(&json!({
                "username": to,
                "body": body
            }))
            .send()
            .await?;
        Ok(())
    }
}
