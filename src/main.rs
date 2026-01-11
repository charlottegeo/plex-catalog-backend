use crate::models::Item;
use crate::plex_client::PlexClient;
use actix_web::{web, App, HttpServer};
use bytes::Bytes;
use futures::stream::{self, StreamExt};
use moka::future::Cache;
use sqlx::postgres::PgPoolOptions;
use std::future::Future;
use std::io::Result;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

mod auth;
mod db;
mod error;
mod models;
mod plex_client;
mod routes;

//Hours between syncs
const SYNC_INTERVAL_HOURS: u64 = 12;

//Library types for syncing, others (music, photos, etc.) are ignored
const SUPPORTED_LIBRARY_TYPES: &[&str] = &["movie", "show"];

//Shared application state
pub struct AppState {
    pub plex_client: PlexClient,
    pub db_pool: sqlx::PgPool,
    pub image_cache: Cache<String, Bytes>,
    pub sync_semaphore: Arc<Semaphore>,
}

//Recursively sync a library item and its children into database
fn sync_item_and_children<'a>(
    state: &'a AppState,
    client: &'a PlexClient,
    db_pool: &'a sqlx::PgPool,
    server_uri: &'a str,
    server_token: &'a str,
    server_id: &'a str,
    library_key: &'a str,
    item: &'a Item,
    sync_time: chrono::DateTime<chrono::Utc>,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let permit = state
            .sync_semaphore
            .acquire()
            .await
            .map_err(|e| {
                tracing::error!("Semaphore closed: {:?}", e);
            })
            .unwrap();
        if let Err(e) = db::upsert_item(db_pool, item, library_key, server_id, sync_time).await {
            tracing::error!(
                "Failed to upsert item '{}': {:?}. Skipping its children.",
                item.title,
                e
            );
            return;
        }
        match item.item_type.as_str() {
            "show" => {
                drop(permit); 
                let mut children_result = client
                    .get_item_children(server_uri, server_token, &item.rating_key)
                    .await;
                let mut use_fallback = false;
                if let Ok(children) = &children_result {
                    if children.items.is_empty()
                        || children.items.iter().any(|i| i.item_type == "episode")
                    {
                        use_fallback = true;
                    }
                }
                if use_fallback {
                    tracing::info!(
                        "'{}' appears flattened or empty. Using /allLeaves.",
                        item.title
                    );
                    children_result = client
                        .get_item_all_leaves(server_uri, server_token, &item.rating_key)
                        .await;
                }
                match children_result {
                    Ok(children) => {
                        stream::iter(children.items)
                            .for_each_concurrent(5, |child_item| {
                                let client_clone = client.clone();
                                let db_pool_clone = db_pool.clone();
                                let server_id_clone = server_id.to_string();
                                let library_key_clone = library_key.to_string();
                                let item_rating_key_clone = item.rating_key.clone();

                                async move {
                                    let mut episode_item = child_item.clone();
                                    if child_item.item_type == "episode"
                                        && episode_item.parent_id.is_none()
                                    {
                                        episode_item.parent_id = Some(item_rating_key_clone);
                                    }
                                    sync_item_and_children(
                                        state,
                                        &client_clone,
                                        &db_pool_clone,
                                        server_uri,
                                        server_token,
                                        &server_id_clone,
                                        &library_key_clone,
                                        &episode_item,
                                        sync_time,
                                    )
                                    .await;
                                }
                            })
                            .await;
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to get children for show '{}': {:?}",
                            item.title,
                            e
                        );
                    }
                }
            }
            "season" => {
                drop(permit);
                let children_result = client
                    .get_item_children(server_uri, server_token, &item.rating_key)
                    .await;
                match children_result {
                    Ok(children) => {
                        stream::iter(children.items)
                            .for_each_concurrent(5, |child_item| {
                                let client_clone = client.clone();
                                let db_pool_clone = db_pool.clone();
                                let server_id_clone = server_id.to_string();
                                let library_key_clone = library_key.to_string();
                                async move {
                                    sync_item_and_children(
                                        state,
                                        &client_clone,
                                        &db_pool_clone,
                                        server_uri,
                                        server_token,
                                        &server_id_clone,
                                        &library_key_clone,
                                        &child_item,
                                        sync_time,
                                    )
                                    .await;
                                }
                            })
                            .await;
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to get children for season '{}': {:?}",
                            item.title,
                            e
                        );
                    }
                }
            }
            "movie" | "episode" => {
                let mut attempts = 0;
                let details_result = loop {
                    match client
                        .get_item_details(server_uri, server_token, &item.rating_key)
                        .await
                    {
                        Ok(res) => break Ok(res),
                        Err(e) => {
                            attempts += 1;
                            if attempts >= 3 {
                                break Err(e);
                            }
                            tokio::time::sleep(Duration::from_millis(500 * attempts)).await;
                        }
                    }
                };
                match details_result {
                    Ok(Some(details)) => {
                        if let Some(media) = details.media.first() {
                            if let Some(part) = media.parts.first() {
                                if let Err(e) = db::upsert_media_part(
                                    db_pool,
                                    part,
                                    &item.rating_key,
                                    server_id,
                                    media,
                                    sync_time,
                                )
                                .await
                                {
                                    tracing::error!(
                                        "Failed to upsert media part for item '{}': {:?}",
                                        item.title,
                                        e
                                    );
                                } else {
                                    stream::iter(&part.streams)
                                        .for_each_concurrent(5, |stream| {
                                            let db_pool_clone = db_pool.clone();
                                            let server_id_clone = server_id.to_string();

                                            async move {
                                                if let Err(e) = db::upsert_stream(
                                                    &db_pool_clone,
                                                    stream,
                                                    part.id,
                                                    &server_id_clone,
                                                    sync_time,
                                                )
                                                .await
                                                {
                                                    tracing::error!(
                                                        "Failed to upsert stream for item '{}': {:?}",
                                                        item.title,
                                                        e
                                                    );
                                                }
                                            }
                                        })
                                        .await;
                                }
                            }
                        }
                    }
                    Ok(None) => tracing::warn!("No details found for item '{}'", item.title),
                    Err(e) => {
                        tracing::error!("Failed to get details for item '{}': {:?}", item.title, e)
                    }
                }
            }
            _ => {
                drop(permit); 
            }
        }
    })
}

async fn sync_server(
    state: web::Data<AppState>,
    server: models::Device,
    client: PlexClient,
    db_pool: sqlx::PgPool,
    sync_start_time: chrono::DateTime<chrono::Utc>,
) {
    let remote_conn = server.connections.iter().find(|c| !c.local);
    let server_token = server.access_token.as_ref();

    if let (Some(conn), Some(token)) = (remote_conn, server_token) {
        let mut attempts = 0;
        let max_attempts = 3;
        let mut delay = Duration::from_secs(2);

        let libraries_result = loop {
            match client.get_libraries(&conn.uri, token).await {
                Ok(libraries) => break Ok(libraries),
                Err(e) => {
                    attempts += 1;
                    if attempts >= max_attempts {
                        tracing::error!(
                            "Server '{}' is offline after {} attempts. Marking as such. Error: {:?}",
                            server.name,
                            max_attempts,
                            e
                        );
                        if db::upsert_server(&db_pool, &server, false, sync_start_time)
                            .await
                            .is_err()
                        {
                            tracing::error!("Failed to mark server '{}' as offline.", server.name);
                        }
                        break Err(e);
                    }
                    tracing::warn!(
                        "Failed to connect to server '{}'. Retrying in {:?}...",
                        server.name,
                        delay
                    );
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
            }
        };

        if let Ok(library_list) = libraries_result {
            if let Err(e) = db::upsert_server(&db_pool, &server, true, sync_start_time).await {
                tracing::error!("Failed to mark server '{}' as online: {}", server.name, e);
                return;
            }

            tracing::info!("Syncing server: {}", server.name);
            for library in &library_list.libraries {
                if SUPPORTED_LIBRARY_TYPES.contains(&library.library_type.as_str()) {
                    tracing::info!("Syncing library: {}", library.title);
                    if db::upsert_library(
                        &db_pool,
                        library,
                        &server.client_identifier,
                        sync_start_time,
                    )
                    .await
                    .is_err()
                    {
                        tracing::error!("Failed to upsert library '{}'", library.title);
                        continue;
                    }

                    let items_result = client
                        .get_library_items(&conn.uri, token, &library.key)
                        .await;

                    if let Ok(item_list) = items_result {
                        tracing::info!(
                            "Found {} items in library '{}'",
                            item_list.items.len(),
                            library.title
                        );

                        stream::iter(item_list.items)
                            .for_each_concurrent(10, |item| {
                                let client_clone = client.clone();
                                let db_pool_clone = db_pool.clone();
                                let server_id_clone = server.client_identifier.clone();
                                let library_key_clone = library.key.clone();
                                let state_clone = state.clone();
                                async move {
                                    sync_item_and_children(
                                        &state_clone,
                                        &client_clone,
                                        &db_pool_clone,
                                        &conn.uri,
                                        token,
                                        &server_id_clone,
                                        &library_key_clone,
                                        &item,
                                        sync_start_time,
                                    )
                                    .await;
                                }
                            })
                            .await;
                    } else {
                        tracing::error!("FAILED to get items for library '{}'", library.title);
                    }
                }
            }

            tracing::info!("Successfully synced server '{}'", server.name);
        }
    } else {
        tracing::warn!(
            "Skipping server '{}' (no remote connection or token).",
            server.name
        );
        if db::upsert_server(&db_pool, &server, false, sync_start_time)
            .await
            .is_err()
        {
            tracing::error!("Failed to mark server '{}' as offline.", server.name);
        }
    }
}

async fn run_database_sync(app_state: &web::Data<AppState>) {
    let sync_start_time = chrono::Utc::now();
    let client = app_state.plex_client.clone();
    let db_pool = app_state.db_pool.clone();

    if let Err(e) = client.ensure_logged_in().await {
        tracing::error!("Failed to log in to Plex: {:?}", e);
        return;
    }

    let servers_result = client.get_servers().await;

    if let Ok(servers) = servers_result {
        tracing::info!("Syncing {} online servers...", servers.len());
        stream::iter(servers)
            .for_each_concurrent(3, |server| {
                sync_server(
                    app_state.clone(),
                    server,
                    client.clone(),
                    db_pool.clone(),
                    sync_start_time,
                )
            })
            .await;
    } else {
        tracing::error!("Failed to get initial server list.");
    }

    tracing::info!("Sync loop finished. Pruning old data...");
    if let Err(e) = db::prune_old_data(&db_pool, sync_start_time).await {
        tracing::error!("Failed to prune old data: {:?}", e);
    }
}

async fn database_sync_scheduler(app_state: web::Data<AppState>) {
    tracing::info!("Performing initial database sync on startup...");
    run_database_sync(&app_state).await;
    tracing::info!("Initial sync complete. Starting scheduled runs.");

    let mut interval = tokio::time::interval(Duration::from_secs(60 * 60 * SYNC_INTERVAL_HOURS));
    interval.tick().await;

    loop {
        interval.tick().await;
        tracing::info!("Starting scheduled database sync...");
        run_database_sync(&app_state).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let db_pool = PgPoolOptions::new()
        .max_connections(15)
        .connect(&database_url)
        .await
        .expect("Failed to connect to Postgres.");
    tracing::info!("Successfully connected to database.");

    let plex_client = PlexClient::new();

    let image_cache = Cache::builder()
        .max_capacity(500 * 1024 * 1024)
        .time_to_live(Duration::from_secs(12 * 60 * 60))
        .build();

    let app_state = web::Data::new(AppState {
        plex_client: plex_client.clone(),
        db_pool: db_pool.clone(),
        image_cache,
        sync_semaphore: Arc::new(Semaphore::new(25)),
    });

    tokio::spawn(database_sync_scheduler(app_state.clone()));
    tracing::info!("Backend server starting on http://0.0.0.0:3001");

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone())
            .configure(routes::configure)
    })
    .bind(("0.0.0.0", 3001))?
    .run()
    .await
}
