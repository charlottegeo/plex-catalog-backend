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
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

mod auth;
mod db;
mod error;
mod models;
mod plex_client;
mod routes;

const SYNC_INTERVAL_HOURS: u64 = 12;
const SUPPORTED_LIBRARY_TYPES: &[&str] = &["movie", "show"];

#[derive(Clone)]
pub struct AppState {
    pub plex_client: PlexClient,
    pub db_pool: sqlx::PgPool,
    pub image_cache: Cache<String, Bytes>,
    pub sync_semaphore: Arc<Semaphore>,
}

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
        let _permit = match state.sync_semaphore.acquire().await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Critical: Sync semaphore closed unexpectedly: {:?}", e);
                return;
            }
        };

        if let Err(e) = db::upsert_item(db_pool, item, library_key, server_id, sync_time).await {
            tracing::error!(
                "Database Error: Failed to upsert item '{}' (Type: {}): {:?}",
                item.title,
                item.item_type,
                e
            );
            return;
        }

        match item.item_type.as_str() {
            "show" => {
                drop(_permit);
                if let Ok(children) = client
                    .get_item_children(server_uri, server_token, &item.rating_key)
                    .await
                {
                    for child in children.items {
                        let mut child_to_sync = child.clone();
                        if child.item_type == "episode" {
                            child_to_sync.parent_id = Some(item.rating_key.clone());
                        }
                        sync_item_and_children(
                            state,
                            client,
                            db_pool,
                            server_uri,
                            server_token,
                            server_id,
                            library_key,
                            &child_to_sync,
                            sync_time,
                        )
                        .await;
                    }
                } else {
                    tracing::warn!(
                        "Sync Warning: Could not fetch seasons for show '{}'",
                        item.title
                    );
                }

                if item.leaf_count.unwrap_or(0) > 0 {
                    if let Ok(leaves) = client
                        .get_item_all_leaves(server_uri, server_token, &item.rating_key)
                        .await
                    {
                        stream::iter(leaves.items)
                            .for_each_concurrent(1, |leaf_item| {
                                let state_c = state.clone();
                                let client_c = client.clone();
                                let db_p = db_pool.clone();
                                let s_id = server_id.to_string();
                                let l_key = library_key.to_string();
                                let p_id = item.rating_key.clone();
                                async move {
                                    let mut episode_item = leaf_item.clone();
                                    if leaf_item.item_type == "episode"
                                        && leaf_item.parent_id.is_none()
                                    {
                                        episode_item.parent_id = Some(p_id);
                                    }
                                    sync_item_and_children(
                                        &state_c,
                                        &client_c,
                                        &db_p,
                                        server_uri,
                                        server_token,
                                        &s_id,
                                        &l_key,
                                        &episode_item,
                                        sync_time,
                                    )
                                    .await;
                                }
                            })
                            .await;
                    } else {
                        tracing::warn!(
                            "Sync Warning: Could not fetch episodes for show '{}'",
                            item.title
                        );
                    }
                }
            }
            "season" => {
                drop(_permit);
            }
            "movie" | "episode" => {
                if let Ok(Some(details)) = client
                    .get_item_details(server_uri, server_token, &item.rating_key)
                    .await
                {
                    if let Some(media) = details.media.first() {
                        if let Some(part) = media.parts.first() {
                            if db::upsert_media_part(
                                db_pool,
                                part,
                                &item.rating_key,
                                server_id,
                                media,
                                sync_time,
                            )
                            .await
                            .is_ok()
                            {
                                stream::iter(&part.streams)
                                    .for_each_concurrent(2, |stream| {
                                        let db_p = db_pool.clone();
                                        let s_id = server_id.to_string();
                                        async move {
                                            if let Err(e) = db::upsert_stream(
                                                &db_p, stream, part.id, &s_id, sync_time,
                                            )
                                            .await {
                                                tracing::error!("Database Error: Failed to upsert stream for part {}: {:?}", part.id, e);
                                            }
                                        }
                                    })
                                    .await;
                            }
                        }
                    }
                } else {
                    tracing::warn!(
                        "Sync Warning: Details not found for {} '{}'",
                        item.item_type,
                        item.title
                    );
                }
            }
            _ => {
                drop(_permit);
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
        tracing::info!("Starting sync for server: '{}'", server.name);
        if let Ok(library_list) = client.get_libraries(&conn.uri, token).await {
            let _ = db::upsert_server(&db_pool, &server, true, sync_start_time).await;

            for library in &library_list.libraries {
                if SUPPORTED_LIBRARY_TYPES.contains(&library.library_type.as_str()) {
                    tracing::info!(
                        "Syncing library: '{}' on server: '{}'",
                        library.title,
                        server.name
                    );
                    let _ = db::upsert_library(
                        &db_pool,
                        library,
                        &server.client_identifier,
                        sync_start_time,
                    )
                    .await;

                    if let Ok(item_list) = client
                        .get_library_items(&conn.uri, token, &library.key)
                        .await
                    {
                        tracing::info!(
                            "Processing {} items from library '{}'",
                            item_list.items.len(),
                            library.title
                        );
                        stream::iter(item_list.items)
                            .for_each_concurrent(1, |item| {
                                let state_c = state.clone();
                                let client_c = client.clone();
                                let db_p = db_pool.clone();
                                let s_id = server.client_identifier.clone();
                                let l_key = library.key.clone();
                                let uri = conn.uri.clone();
                                let tok = token.to_string();
                                async move {
                                    sync_item_and_children(
                                        &state_c,
                                        &client_c,
                                        &db_p,
                                        &uri,
                                        &tok,
                                        &s_id,
                                        &l_key,
                                        &item,
                                        sync_start_time,
                                    )
                                    .await;
                                }
                            })
                            .await;
                    } else {
                        tracing::error!(
                            "Fetch Error: Failed to get items for library '{}'",
                            library.title
                        );
                    }
                }
            }
            tracing::info!("Finished sync for server: '{}'", server.name);
        } else {
            tracing::error!(
                "Connection Error: Failed to reach libraries for server '{}'",
                server.name
            );
        }
    } else {
        tracing::warn!(
            "Skip: Server '{}' has no valid remote connection or token",
            server.name
        );
    }
}

async fn run_database_sync(app_state: &web::Data<AppState>) {
    let start_instant = Instant::now();
    let sync_start_time = chrono::Utc::now();
    let client = app_state.plex_client.clone();
    let db_pool = app_state.db_pool.clone();

    tracing::info!("Initiating database sync run at {}", sync_start_time);

    if let Err(e) = client.ensure_logged_in().await {
        tracing::error!("Auth Error: Failed Plex login. Sync aborted: {:?}", e);
        return;
    }

    if let Ok(servers) = client.get_servers().await {
        tracing::info!("Discovered {} servers to sync", servers.len());
        stream::iter(servers)
            .for_each_concurrent(1, |server| {
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
        tracing::error!("Network Error: Failed to retrieve server list from Plex API");
    }

    tracing::info!("Sync complete. Pruning orphaned data...");
    if let Err(e) = db::prune_old_data(&db_pool, sync_start_time).await {
        tracing::error!("Database Error: Data pruning failed: {:?}", e);
    }

    let duration = start_instant.elapsed();
    tracing::info!(
        "Database sync run finished successfully. Total duration: {}.{:03}s",
        duration.as_secs(),
        duration.subsec_millis()
    );
}

async fn database_sync_scheduler(app_state: web::Data<AppState>) {
    tracing::info!("Service started. Performing initial database sync...");
    run_database_sync(&app_state).await;

    let mut interval = tokio::time::interval(Duration::from_secs(60 * 60 * SYNC_INTERVAL_HOURS));

    interval.tick().await;

    loop {
        tracing::info!("Next scheduled sync in {} hours.", SYNC_INTERVAL_HOURS);
        interval.tick().await;
        run_database_sync(&app_state).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("Starting backend...");

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let db_pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .expect("Failed to connect to Postgres.");

    tracing::info!("Postgres connected.");

    let app_state = web::Data::new(AppState {
        plex_client: PlexClient::new(),
        db_pool: db_pool.clone(),
        image_cache: Cache::builder()
            .max_capacity(500 * 1024 * 1024)
            .time_to_live(Duration::from_secs(12 * 60 * 60))
            .build(),
        sync_semaphore: Arc::new(Semaphore::new(3)),
    });

    tokio::spawn(database_sync_scheduler(app_state.clone()));

    tracing::info!("HTTP server starting on 0.0.0.0:3001");

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone())
            .configure(routes::configure)
    })
    .workers(2)
    .bind(("0.0.0.0", 3001))?
    .run()
    .await
}
