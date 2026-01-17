use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct User {
    #[serde(rename = "authToken")]
    pub auth_token: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LoginResponse {
    pub user: User,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Device {
    pub name: String,
    pub product: String,
    pub provides: String,
    #[serde(rename = "clientIdentifier")]
    pub client_identifier: String,
    #[serde(rename = "accessToken")]
    pub access_token: Option<String>,
    pub connections: Vec<Connection>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Connection {
    pub uri: String,
    pub local: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LibraryMediaContainer {
    #[serde(rename = "MediaContainer")]
    pub media_container: LibraryList,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LibraryList {
    #[serde(rename = "Directory")]
    pub libraries: Vec<Library>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Library {
    pub key: String,
    pub title: String,
    #[serde(rename = "type")]
    pub library_type: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ItemMediaContainer {
    #[serde(rename = "MediaContainer")]
    pub media_container: ItemList,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ItemList {
    #[serde(rename = "Metadata")]
    pub items: Vec<Item>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Item {
    pub guid: Option<String>,
    #[serde(rename = "ratingKey")]
    pub rating_key: String,
    #[serde(rename = "parentRatingKey")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub index: Option<i32>,
    #[serde(default)]
    #[serde(rename = "leafCount")]
    pub leaf_count: Option<i32>,
    pub title: String,
    pub key: String,
    pub summary: String,
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default)]
    pub year: u16,
    pub thumb: Option<String>,
    pub art: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SingleItemMediaContainer {
    #[serde(rename = "MediaContainer")]
    pub media_container: SingleItemList,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SingleItemList {
    #[serde(rename = "Metadata")]
    pub items: Vec<ItemWithDetails>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ItemWithDetails {
    pub guid: Option<String>,
    #[serde(rename = "ratingKey")]
    pub rating_key: String,
    #[serde(rename = "parentRatingKey")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub index: Option<i32>,
    #[serde(default)]
    #[serde(rename = "leafCount")]
    pub leaf_count: Option<i32>,
    pub title: String,
    pub key: String,
    pub summary: String,
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default)]
    pub year: u16,
    #[serde(rename = "Media", default)]
    pub media: Vec<Media>,
    pub thumb: Option<String>,
    pub art: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Media {
    #[serde(rename = "videoResolution")]
    pub video_resolution: Option<String>,
    #[serde(rename = "Part", default)]
    pub parts: Vec<Part>,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Part {
    pub id: i64,
    #[serde(rename = "Stream", default)]
    pub streams: Vec<Stream>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Stream {
    pub id: i64,
    #[serde(rename = "streamType")]
    pub stream_type: u8,
    pub language: Option<String>,
    pub language_code: Option<String>,
    pub format: Option<String>,
}

#[derive(Serialize, FromRow, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DbServer {
    pub id: String,
    pub name: String,
    pub is_online: bool,
    pub access_token: String,
    pub connection_uri: String,
    pub last_seen: DateTime<Utc>,
}

#[derive(Serialize, FromRow, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub guid: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub item_type: String,
    pub year: Option<i16>,
    pub thumb_path: Option<String>,
    pub server_id: String,
    pub server_name: String,
    #[serde(skip_serializing)]
    pub rank: Option<f32>,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MediaVersion {
    pub video_resolution: String,
    pub subtitles: Vec<String>,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServerAvailability {
    pub server_id: String,
    pub server_name: String,
    pub rating_key: String,
    pub versions: Vec<MediaVersion>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct MediaDetails {
    pub guid: String,
    pub title: String,
    pub summary: Option<String>,
    pub year: Option<i16>,
    pub art_path: Option<String>,
    pub thumb_path: Option<String>,
    pub item_type: String,
    pub available_on: Vec<ServerAvailability>,
}

#[derive(Serialize, FromRow, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SeasonSummary {
    pub id: String,
    pub title: String,
    pub summary: Option<String>,
    pub thumb_path: Option<String>,
    pub art_path: Option<String>,
    #[serde(rename = "episodeCount")]
    pub leaf_count: Option<i32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeDetails {
    pub id: String,
    pub title: String,
    pub summary: Option<String>,
    pub thumb_path: Option<String>,
    pub index: Option<i32>,
    pub versions: Vec<MediaVersion>,
}

impl From<DbServer> for Device {
    fn from(db_server: DbServer) -> Self {
        Device {
            name: db_server.name,
            product: "".to_string(),
            provides: "server".to_string(),
            client_identifier: db_server.id,
            access_token: Some(db_server.access_token),
            connections: vec![Connection {
                uri: db_server.connection_uri,
                local: false,
            }],
        }
    }
}
