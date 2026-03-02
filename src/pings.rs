use std::env;
use crate::models::PingsBody;
use anyhow::anyhow;
use isahc::{Request, RequestExt};

pub fn send_ping(username: String, body: String) -> Result<(), anyhow::Error> {
    let secret = env::var("PINGS_SECRET")?;
    let route = env::var("PINGS_ROUTE")?;
    let response = Request::post(format!(
        "https://pings.csh.rit.edu/service/route/{route}/ping"
    ))
    .header("Authorization", format!("Bearer {}", secret))
    .header("Content-Type", "application/json")
    .body(serde_json::to_vec(&PingsBody { body, username })?)?
    .send()?;

    if response.status().is_success() {
        Ok(())
    } else {
        Err(anyhow!("Failed to ping."))
    }
}
    