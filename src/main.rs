use crate::plex_client::PlexClient;
use actix_web::{web, App, HttpServer};
use std::io::Result;
use std::sync::Arc;
use tokio::sync::Mutex;

mod error;
mod models;
mod plex_client;
mod routes;

pub struct AppState {
    pub plex_client: Arc<Mutex<PlexClient>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().expect("Failed to read .env file");
    let plex_client = Arc::new(Mutex::new(PlexClient::new()));
    let app_state = web::Data::new(AppState { plex_client });

    println!("Backend server starting on http://127.0.0.1:3001");

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone())
            .configure(routes::configure)
    })
    .bind(("127.0.0.1", 3001))?
    .run()
    .await
}
