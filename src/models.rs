use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;

fn deserialize_opt_naive_date<'de, D>(d: D) -> Result<Option<NaiveDate>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(d)?;
    Ok(opt.and_then(|s| {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
    }))
}

/// Plex user with authentication token.
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct User {
    #[serde(rename = "authToken")]
    pub auth_token: String,
}

/// Plex sign-in response containing user info.
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct LoginResponse {
    pub user: User,
}

/// Plex device (server) with information needed to connect to the server.
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
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

/// Plex server connection URI, and whether it is a local connection.
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct Connection {
    pub uri: String,
    pub local: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct LibraryMediaContainer {
    #[serde(rename = "MediaContainer")]
    pub media_container: LibraryList,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct LibraryList {
    #[serde(rename = "Directory")]
    pub libraries: Vec<Library>,
}

/// Plex library section (movies, shows, etc).
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct Library {
    pub key: String,
    pub title: String,
    #[serde(rename = "type")]
    pub library_type: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct ItemMediaContainer {
    #[serde(rename = "MediaContainer")]
    pub media_container: ItemList,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct ItemList {
    #[serde(rename = "Metadata")]
    pub items: Vec<Item>,
}

/// Plex library item (movie, show, episode, extra, etc).
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
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
    pub summary: Option<String>,
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default)]
    pub year: u16,
    #[serde(rename = "Media", default)]
    pub media: Vec<Media>,
    pub thumb: Option<String>,
    pub art: Option<String>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<i64>,
    #[serde(rename = "contentRating", default)]
    pub content_rating: Option<String>,
    #[serde(default)]
    pub duration: Option<i64>,
    #[serde(
        rename = "originallyAvailableAt",
        default,
        deserialize_with = "deserialize_opt_naive_date"
    )]
    #[schema(value_type = Option<String>)]
    pub originally_available_at: Option<NaiveDate>,
    #[serde(default)]
    pub studio: Option<String>,
    /// Plex extra type (e.g. "trailer", "behindTheScenes") when item_type is "extra".
    #[serde(rename = "extraType", default)]
    pub extra_type: Option<String>,
}

/// Single bonus feature/extra from the database.
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlexExtra {
    #[serde(rename = "ratingKey")]
    pub rating_key: Option<String>,
    pub title: String,
    pub key: String,
    /// Plex extra type (e.g. "trailer", "behindTheScenes", "deleted_scene").
    #[serde(rename = "extraType")]
    pub extra_type: Option<String>,
    pub thumb: Option<String>,
}

/// Container for extras returned by the database.
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct PlexExtrasContainer {
    #[serde(rename = "Metadata", alias = "Directory", default)]
    pub metadata: Vec<PlexExtra>,
}

/// Response wrapper for Plex extras from the Plex API.
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct PlexExtrasResponse {
    #[serde(rename = "MediaContainer")]
    pub media_container: PlexExtrasContainer,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct SingleItemMediaContainer {
    #[serde(rename = "MediaContainer")]
    pub media_container: SingleItemList,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct SingleItemList {
    #[serde(rename = "Metadata")]
    pub items: Vec<ItemWithDetails>,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
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
    pub summary: Option<String>,
    #[serde(rename = "type")]
    pub item_type: String,
    #[serde(default)]
    pub year: u16,
    #[serde(rename = "Media", default)]
    pub media: Vec<Media>,
    pub thumb: Option<String>,
    pub art: Option<String>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<i64>,
    #[serde(rename = "contentRating", default)]
    pub content_rating: Option<String>,
    #[serde(default)]
    pub duration: Option<i64>,
    #[serde(
        rename = "originallyAvailableAt",
        default,
        deserialize_with = "deserialize_opt_naive_date"
    )]
    #[schema(value_type = Option<String>)]
    pub originally_available_at: Option<NaiveDate>,
    #[serde(default)]
    pub studio: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct Media {
    #[serde(rename = "videoResolution")]
    pub video_resolution: Option<String>,
    #[serde(rename = "Part", default)]
    pub parts: Vec<Part>,
}
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct Part {
    pub id: i64,
    #[serde(rename = "Stream", default)]
    pub streams: Vec<Stream>,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
pub struct Stream {
    pub id: i64,
    #[serde(rename = "streamType")]
    pub stream_type: u8,
    pub language: Option<String>,
    pub language_code: Option<String>,
    pub format: Option<String>,
}

/// Information about the database and sync status.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    #[schema(value_type = Option<String>)]
    pub last_updated: Option<DateTime<Utc>>,
    pub sync_interval_minutes: u64,
    pub total_movies: i64,
    pub total_shows: i64,
    pub online_servers: i64,
    pub offline_servers: i64,
    /// Last sync error message if any.
    pub last_error: Option<String>,
    /// Whether a sync is currently in progress.
    pub sync_in_progress: bool,
}

#[derive(Serialize, FromRow, Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DbServer {
    pub id: String,
    pub name: String,
    pub is_online: bool,
    pub access_token: String,
    pub connection_uri: String,
    #[schema(value_type = String)]
    pub last_seen: DateTime<Utc>,
}

#[derive(Serialize, FromRow, Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub rating_key: String,
    pub guid: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub item_type: String,
    pub year: Option<i16>,
    #[schema(value_type = Option<String>)]
    pub originally_available_at: Option<chrono::NaiveDate>,
    pub thumb_path: Option<String>,
    pub server_id: String,
    pub server_name: String,
    pub content_rating: Option<String>,
    pub duration: Option<i64>,
    #[allow(dead_code)]
    pub rank: Option<f32>,
}

#[derive(Deserialize, ToSchema)]
pub struct SearchQuery {
    pub q: String,
}

#[derive(Deserialize, ToSchema)]
pub struct ImageQuery {
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Clone)]
pub struct CachedImage {
    pub bytes: bytes::Bytes,
    pub content_type: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaVersion {
    pub video_resolution: String,
    pub subtitles: Vec<String>,
}
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerAvailability {
    pub server_id: String,
    pub server_name: String,
    pub rating_key: String,
    pub access_token: String,
    pub versions: Vec<MediaVersion>,
}

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaDetails {
    pub guid: String,
    pub title: String,
    pub summary: Option<String>,
    pub year: Option<i16>,
    #[schema(value_type = Option<String>)]
    pub originally_available_at: Option<chrono::NaiveDate>,
    pub art_path: Option<String>,
    pub thumb_path: Option<String>,
    pub item_type: String,
    pub content_rating: Option<String>,
    pub duration: Option<i64>,
    pub available_on: Vec<ServerAvailability>,
}

#[derive(Serialize, FromRow, Debug, Clone, ToSchema)]
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

#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeDetails {
    pub id: String,
    pub title: String,
    pub summary: Option<String>,
    pub thumb_path: Option<String>,
    pub index: Option<i32>,
    pub content_rating: Option<String>,
    pub duration: Option<i64>,
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

#[derive(Serialize, Deserialize, Debug, ToSchema)]
#[allow(dead_code)]
pub struct PingsBody {
    pub username: String,
    pub body: String,
}

#[derive(Serialize, Deserialize, Debug, ToSchema)]
#[allow(dead_code)]
pub struct MediaRequest {
    pub guid: String,
    pub title: String,
    pub item_type: String,
    pub year: Option<i16>,
    pub seasons: Option<Vec<i32>>,
    pub resolution: Option<String>,
    pub username: String,
}

/// Response from the Plex playQueues API for instant playback of media.
#[derive(Serialize, Deserialize, Debug, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlayQueueResponse {
    #[serde(rename = "playQueueID")]
    pub play_queue_id: i64,
    #[serde(rename = "Metadata", default)]
    pub metadata: Vec<serde_json::Value>,
}

/// Container wrapper for the Plex playQueues API response.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlayQueueContainer {
    #[serde(rename = "MediaContainer")]
    pub media_container: PlayQueueResponse,
}
