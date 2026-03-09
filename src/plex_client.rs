use crate::models::{
    Device, ItemList, ItemMediaContainer, ItemWithDetails, LibraryList, LibraryMediaContainer,
    LoginResponse, PlayQueueContainer, PlexExtra, SingleItemMediaContainer,
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

    /// Checks Plex API connectivity by pinging plex.tv.
    /// Returns Ok(()) if reachable, Err if unreachable.
    pub async fn check_connectivity(&self) -> Result<(), reqwest::Error> {
        let _ = self
            .http_client
            .get("https://plex.tv/pms/:/ip")
            .header("X-Plex-Product", "Plex Catalog Web")
            .header("X-Plex-Client-Identifier", &self.client_identifier)
            .send()
            .await?
            .error_for_status()?;

        self.get_servers().await?;
        Ok(())
    }

    /// Fetches extras for a media item (movie, show, season, episode) from the metadata endpoint with includeExtras=1.
    /// Extras are embedded in MediaContainer.Metadata[0].Extras.Metadata
    pub async fn get_item_extras(
        &self,
        server_uri: &str,
        server_token: &str,
        rating_key: &str,
    ) -> Result<Vec<PlexExtra>, reqwest::Error> {
        let response = self
            .http_client
            .get(format!(
                "{}/library/metadata/{}?includeExtras=1",
                server_uri.trim_end_matches('/'),
                rating_key
            ))
            .header("Accept", "application/json")
            .header("X-Plex-Token", server_token)
            .header("X-Plex-Client-Identifier", &self.client_identifier)
            .send()
            .await?
            .error_for_status()?;

        let value: serde_json::Value = response.json().await?;
        let extras = value
            .get("MediaContainer")
            .and_then(|mc| mc.get("Metadata"))
            .and_then(|m| m.get(0))
            .and_then(|first| first.get("Extras"))
            .and_then(|e| e.get("Metadata"))
            .and_then(|arr| arr.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let obj = item.as_object()?;
                        let title = obj.get("title")?.as_str()?.to_string();
                        let key = obj
                            .get("key")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let rating_key = obj
                            .get("ratingKey")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        let thumb = obj.get("thumb").and_then(|v| v.as_str()).map(String::from);
                        let extra_type = obj
                            .get("subtype")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        Some(PlexExtra {
                            rating_key,
                            title,
                            key,
                            extra_type,
                            thumb,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Ok(extras)
    }

    /// Creates a play queue for instant playback. Uses a unique client_identifier to allow
    /// multiple users on the same account to have their own queues.
    pub async fn create_play_queue(
        &self,
        server_uri: &str,
        server_token: &str,
        machine_id: &str,
        rating_key: &str,
        client_identifier: Option<&str>,
    ) -> Result<PlayQueueContainer, reqwest::Error> {
        let client_id = client_identifier.unwrap_or(&self.client_identifier);
        let base = server_uri.trim_end_matches('/');
        let url = format!("{}/playQueues", base);

        let internal_uri = format!(
            "server://{}/com.plexapp.plugins.library/library/metadata/{}",
            machine_id, rating_key
        );

        let response = self
            .http_client
            .post(&url)
            .header("Accept", "application/json")
            .header("X-Plex-Token", server_token)
            .header("X-Plex-Client-Identifier", client_id)
            .header("X-Plex-Product", "Plex Catalog Web")
            .query(&[
                ("uri", internal_uri.as_str()),
                ("type", "video"),
                ("continuous", "1"),
                ("includeChapters", "1"),
                ("includeMarkers", "1"),
            ])
            .send()
            .await?
            .error_for_status()?;

        let container: PlayQueueContainer = response.json().await?;
        Ok(container)
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

    /// Searches Plex's global catalog/discover for movies and TV shows.
    pub async fn search_global_discover(
        &self,
        query: &str,
    ) -> Result<serde_json::Value, reqwest::Error> {
        self.ensure_logged_in().await?;
        let token = self.auth_token.read().await.as_ref().unwrap().clone();

        let url = format!(
            "https://discover.provider.plex.tv/library/search?query={}&limit=30&searchTypes=movies%2Ctv&searchProviders=discover%2CplexAVOD%2CplexTVOD&includeMetadata=1",
            urlencoding::encode(query)
        );

        let response = self
            .http_client
            .get(&url)
            .header("Accept", "application/json")
            .header("X-Plex-Token", token)
            .header("X-Plex-Client-Identifier", &self.client_identifier)
            .header("X-Plex-Product", "Plex Catalog Web")
            .header("X-Plex-Provider-Version", "7.2")
            .header("X-Plex-Language", "en")
            .send()
            .await?;

        if !response.status().is_success() {
            tracing::warn!(
                "Plex Discover API returned non-success status: {}",
                response.status()
            );
            return Ok(serde_json::json!({
                "MediaContainer": { "Metadata": [] }
            }));
        }

        let text_response = response.text().await?;
        let json: serde_json::Value = match serde_json::from_str(&text_response) {
            Ok(j) => j,
            Err(e) => {
                tracing::error!("Failed to parse Plex Discover JSON: {}", e);
                return Ok(serde_json::json!({ "MediaContainer": { "Metadata": [] } }));
            }
        };

        let mut flat_metadata = Vec::new();
        let mut seen_guids = std::collections::HashSet::new();

        if let Some(search_results) = json["MediaContainer"]["SearchResults"].as_array() {
            for bucket in search_results {
                if bucket["id"] == "people" {
                    continue;
                }

                if let Some(results) = bucket["SearchResult"].as_array() {
                    for result in results {
                        if let Some(metadata) = result["Metadata"].as_object() {
                            if let Some(guid) = metadata.get("guid").and_then(|g| g.as_str()) {
                                if !seen_guids.contains(guid) {
                                    seen_guids.insert(guid.to_string());
                                    flat_metadata.push(serde_json::Value::Object(metadata.clone()));
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(serde_json::json!({
            "MediaContainer": {
                "Metadata": flat_metadata
            }
        }))
    }

    pub async fn get_image(
        &self,
        server_uri: &str,
        server_token: &str,
        image_path: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<Response, reqwest::Error> {
        let base_url = server_uri.trim_end_matches('/');

        let url = if let (Some(w), Some(h)) = (width, height) {
            let internal_path = if image_path.starts_with('/') {
                image_path.to_string()
            } else {
                format!("/{}", image_path)
            };

            format!(
                "{}/photo/:/transcode?url={}&width={}&height={}&format=jpeg&minSize=1&upscale=1&X-Plex-Token={}",
                base_url,
                urlencoding::encode(&internal_path),
                w,
                h,
                server_token
            )
        } else {
            format!("{}/{}", base_url, image_path.trim_start_matches('/'))
        };

        self.http_client
            .get(url)
            .header("X-Plex-Token", server_token)
            .send()
            .await?
            .error_for_status()
    }
}
