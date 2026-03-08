use std::env;
use crate::models::PingsBody;
use anyhow::{Result, anyhow};
use reqwest::Client;
use serde_json::json;

pub async fn send_ping(username: String, body: String) -> Result<()> {
    let secret = env::var("PINGS_SECRET")?;
    let route = env::var("PINGS_ROUTE")?;
    let client = Client::new();

    let response = client.post(format!("https://pings.csh.rit.edu/service/route/{route}/ping"))
        .header("Authorization", format!("Bearer {}", secret))
        .json(&PingsBody { body, username })
        .send()
        .await?;

    if response.status().is_success() {
        Ok(())
    } else {
        Err(anyhow!("Failed to ping service: {}", response.status()))
    }
}