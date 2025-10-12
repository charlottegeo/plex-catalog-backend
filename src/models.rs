use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct User {
    #[serde(rename = "authToken")]
    pub auth_token: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LoginResponse {
    pub user: User,
}

#[derive(Serialize, Deserialize, Debug)]
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

#[derive(Serialize, Deserialize, Debug)]
pub struct Connection {
    pub uri: String,
    pub local: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct LibraryMediaContainer {
    #[serde(rename = "MediaContainer")]
    pub media_container: LibraryList,
}
#[derive(Serialize, Deserialize, Debug)]
pub struct LibraryList {
    #[serde(rename = "Directory")]
    pub libraries: Vec<Library>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Library {
    pub key: String,
    pub title: String,
    #[serde(rename = "type")]
    pub library_type: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ItemMediaContainer {
    #[serde(rename = "MediaContainer")]
    pub media_container: ItemList,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ItemList {
    #[serde(rename = "Metadata")]
    pub items: Vec<Item>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Item {
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

#[derive(Serialize, Deserialize, Debug)]
pub struct SingleItemMediaContainer {
    #[serde(rename = "MediaContainer")]
    pub media_container: SingleItemList,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SingleItemList {
    #[serde(rename = "Metadata")]
    pub items: Vec<ItemWithDetails>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ItemWithDetails {
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

#[derive(Serialize, Deserialize, Debug)]
pub struct Media {
    #[serde(rename = "videoResolution")]
    pub video_resolution: String,
    #[serde(rename = "Part", default)]
    pub parts: Vec<Part>,
}
#[derive(Serialize, Deserialize, Debug)]
pub struct Part {
    #[serde(rename = "Stream", default)]
    pub streams: Vec<Stream>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Stream {
    #[serde(rename = "streamType")]
    pub stream_type: u8, //1=video, 2=audio, 3=subtitle
    pub language: Option<String>,
    pub lang_code: Option<String>,
    pub format: Option<String>,
}
