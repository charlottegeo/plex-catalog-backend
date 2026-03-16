use reqwest::Client;
use serde::Deserialize;
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct OidcClient {
    http_client: Client,
    client_id: String,
    client_secret: String,
    authority: String,
    token_cache: Arc<RwLock<Option<(String, Instant)>>>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct KeycloakUser {
    username: String,
}

impl OidcClient {
    pub fn new() -> Self {
        let client_id = env::var("OIDC_CLIENT_ID").expect("OIDC_CLIENT_ID must be set");
        let client_secret = env::var("OIDC_CLIENT_SECRET").expect("OIDC_CLIENT_SECRET must be set");
        let authority = env::var("SSO_AUTHORITY").expect("SSO_AUTHORITY must be set");

        Self {
            http_client: Client::new(),
            client_id,
            client_secret,
            authority,
            token_cache: Arc::new(RwLock::new(None)),
        }
    }

    async fn get_token(&self) -> Result<String, reqwest::Error> {
        {
            let cache = self.token_cache.read().await;
            if let Some((token, expiry)) = &*cache {
                if *expiry > Instant::now() {
                    return Ok(token.clone());
                }
            }
        }

        let token_url = format!(
            "{}/protocol/openid-connect/token",
            self.authority.trim_end_matches('/')
        );

        let params = [
            ("grant_type", "client_credentials"),
            ("client_id", &self.client_id),
            ("client_secret", &self.client_secret),
        ];

        let res = self
            .http_client
            .post(&token_url)
            .form(&params)
            .send()
            .await?
            .error_for_status()?;

        let data: TokenResponse = res.json().await?;

        let mut cache = self.token_cache.write().await;
        *cache = Some((
            data.access_token.clone(),
            Instant::now() + Duration::from_secs(data.expires_in.saturating_sub(10)),
        ));

        Ok(data.access_token)
    }

    pub async fn get_csh_uid_by_plex(
        &self,
        plex_username: &str,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let access_token = self.get_token().await?;

        let admin_url = self.authority.replace("/realms/", "/admin/realms/");
        let users_url = format!("{}/users", admin_url.trim_end_matches('/'));

        let query_param = format!("plex:{}", plex_username);

        let res = self
            .http_client
            .get(&users_url)
            .header("Authorization", format!("Bearer {}", access_token))
            .query(&[("q", query_param)])
            .send()
            .await?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await?;
            return Err(format!("Keycloak API Error: {} - {}", status, body).into());
        }

        let users: Vec<KeycloakUser> = res.json().await?;
        Ok(users.into_iter().next().map(|u| u.username))
    }
}
