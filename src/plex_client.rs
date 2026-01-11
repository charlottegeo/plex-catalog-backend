use crate::models::{
    Device, ItemList, ItemMediaContainer, ItemWithDetails, LibraryList, LibraryMediaContainer,
    LoginResponse, SingleItemMediaContainer,
};
use reqwest::{Client, ClientBuilder, Response};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct PlexClient {
    http_client: Client,
    client_identifier: String,
    auth_token: Arc<RwLock<Option<String>>>,
}

impl PlexClient {
    pub fn new() -> Self {
        let client_identifier = String::from("rust-plex-catalog-backend-uuid");
        let http_client = ClientBuilder::new()
            .danger_accept_invalid_certs(true)
            .connect_timeout(Duration::from_secs(60))
            .timeout(Duration::from_secs(300))
            .build()
            .expect("Failed to build reqwest client");

        PlexClient {
            http_client,
            client_identifier,
            auth_token: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn ensure_logged_in(&self) -> Result<(), reqwest::Error> {
        if self.auth_token.read().await.is_some() {
            return Ok(());
        }
        println!("PlexClient is not logged in. Authenticating...");
        let username = std::env::var("PLEX_USERNAME").expect("PLEX_USERNAME must be set");
        let password = std::env::var("PLEX_PASSWORD").expect("PLEX_PASSWORD must be set");

        let login_body = json!({ "user": { "login": &username, "password": &password } });

        let response = self
            .http_client
            .post("https://plex.tv/users/sign_in.json")
            .header("X-Plex-Product", "Plex Catalog Web")
            .header("X-Plex-Client-Identifier", &self.client_identifier)
            .header("Accept", "application/json")
            .json(&login_body)
            .send()
            .await?;

        let successful_response = response.error_for_status()?;
        let login_data: LoginResponse = successful_response.json().await?;

        let mut token_lock = self.auth_token.write().await;
        *token_lock = Some(login_data.user.auth_token);

        println!("Authentication successful!");
        Ok(())
    }

    pub async fn get_servers(&self) -> Result<Vec<Device>, reqwest::Error> {
        let token_lock = self.auth_token.read().await;
        let token = token_lock
            .as_ref()
            .expect("get_servers called before login");

        let response = self
            .http_client
            .get("https://plex.tv/api/v2/resources")
            .header("X-Plex-Product", "Plex Catalog Web")
            .header("X-Plex-Client-Identifier", &self.client_identifier)
            .header("X-Plex-Token", token)
            .header("Accept", "application/json")
            .send()
            .await?
            .error_for_status()?;

        let devices: Vec<Device> = response.json().await?;
        Ok(devices
            .into_iter()
            .filter(|d| d.provides == "server")
            .collect())
    }

    pub async fn get_libraries(
        &self,
        server_uri: &str,
        server_token: &str,
    ) -> Result<LibraryList, reqwest::Error> {
        let response = self
            .http_client
            .get(format!("{}/library/sections", server_uri))
            .header("Accept", "application/json")
            .header("X-Plex-Token", server_token)
            .send()
            .await?
            .error_for_status()?;

        let container: LibraryMediaContainer = response.json().await?;
        Ok(container.media_container)
    }

    pub async fn get_library_items(
        &self,
        server_uri: &str,
        server_token: &str,
        library_key: &str,
    ) -> Result<ItemList, reqwest::Error> {
        let response = self
            .http_client
            .get(format!(
                "{}/library/sections/{}/all",
                server_uri, library_key
            ))
            .header("Accept", "application/json")
            .header("X-Plex-Token", server_token)
            .send()
            .await?
            .error_for_status()?;

        let container: ItemMediaContainer = response.json().await?;
        Ok(container.media_container)
    }

    pub async fn get_item_details(
        &self,
        server_uri: &str,
        server_token: &str,
        rating_key: &str,
    ) -> Result<Option<ItemWithDetails>, reqwest::Error> {
        let response = self
            .http_client
            .get(format!("{}/library/metadata/{}", server_uri, rating_key))
            .header("Accept", "application/json")
            .header("X-Plex-Token", server_token)
            .send()
            .await?
            .error_for_status()?;

        let container: SingleItemMediaContainer = response.json().await?;
        Ok(container.media_container.items.into_iter().next())
    }

    pub async fn get_item_all_leaves(
        &self,
        server_uri: &str,
        server_token: &str,
        rating_key: &str,
    ) -> Result<ItemList, reqwest::Error> {
        let response = self
            .http_client
            .get(format!(
                "{}/library/metadata/{}/allLeaves",
                server_uri, rating_key
            ))
            .header("Accept", "application/json")
            .header("X-Plex-Token", server_token)
            .send()
            .await?
            .error_for_status()?;

        let container: ItemMediaContainer = response.json().await?;
        Ok(container.media_container)
    }

    pub async fn get_item_children(
        &self,
        server_uri: &str,
        server_token: &str,
        rating_key: &str,
    ) -> Result<ItemList, reqwest::Error> {
        let response = self
            .http_client
            .get(format!(
                "{}/library/metadata/{}/children",
                server_uri, rating_key
            ))
            .header("Accept", "application/json")
            .header("X-Plex-Token", server_token)
            .send()
            .await?
            .error_for_status()?;

        let container: ItemMediaContainer = response.json().await?;
        Ok(container.media_container)
    }

    pub async fn get_image(
        &self,
        server_uri: &str,
        server_token: &str,
        image_path: &str,
    ) -> Result<Response, reqwest::Error> {
        let full_image_url = format!(
            "{}/{}",
            server_uri.trim_end_matches('/'),
            image_path.trim_start_matches('/')
        );
        let response = self
            .http_client
            .get(full_image_url)
            .header("X-Plex-Token", server_token)
            .send()
            .await?
            .error_for_status()?;
        Ok(response)
    }
}
