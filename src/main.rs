use crate::models::Item;
use crate::plex_client::PlexClient;
use actix_web::{web, App, HttpServer};
use sqlx::postgres::PgPoolOptions;
use std::future::Future;
use std::io::Result;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
mod auth;
mod db;
mod error;
mod models;
mod plex_client;
mod routes;

const SYNC_INTERVAL_HOURS: u64 = 12;

const SUPPORTED_LIBRARY_TYPES: &[&str] = &["movie", "show"];

pub struct AppState {
    pub plex_client: Arc<Mutex<PlexClient>>,
    pub db_pool: sqlx::PgPool,
}

fn sync_item_and_children<'a>(
    client_arc: &'a Arc<Mutex<PlexClient>>,
    tx: &'a mut sqlx::Transaction<'_, sqlx::Postgres>,
    server_uri: &'a str,
    server_token: &'a str,
    server_id: &'a str,
    library_key: &'a str,
    item: &'a Item,
    sync_time: chrono::DateTime<chrono::Utc>,
) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        if let Err(e) = db::upsert_item(tx, item, library_key, server_id, sync_time).await {
            log::error!(
                "Failed to upsert item '{}': {:?}. Skipping its children.",
                item.title, e
            );
            return;
        }
        match item.item_type.as_str() {
            "show" => {
                let mut children_result = {
                    let client = client_arc.lock().await;
                    client
                        .get_item_children(server_uri, server_token, &item.rating_key)
                        .await
                };

                if let Ok(children) = &children_result {
                    if children.items.is_empty() && item.leaf_count.unwrap_or(0) > 0 {
                        log::info!("'{}' has no children via /children but has a leaf_count. Trying /allLeaves fallback.", item.title);
                        children_result = {
                            let client = client_arc.lock().await;
                            client
                                .get_item_all_leaves(server_uri, server_token, &item.rating_key)
                                .await
                        };
                    }
                }

                match children_result {
                    Ok(children) => {
                        for child_item in &children.items {
                            let mut episode_item = child_item.clone();
                            if child_item.item_type == "episode" && child_item.parent_id.is_none() {
                                episode_item.parent_id = Some(item.rating_key.clone());
                            }

                            sync_item_and_children(
                                client_arc,
                                tx,
                                server_uri,
                                server_token,
                                server_id,
                                library_key,
                                &episode_item,
                                sync_time,
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to get children for show '{}': {:?}", item.title, e);
                    }
                }
            }
            "season" => {
                let children_result = {
                    let client = client_arc.lock().await;
                    client
                        .get_item_children(server_uri, server_token, &item.rating_key)
                        .await
                };

                match children_result {
                    Ok(children) => {
                        for child_item in &children.items {
                            sync_item_and_children(
                                client_arc,
                                tx,
                                server_uri,
                                server_token,
                                server_id,
                                library_key,
                                child_item,
                                sync_time,
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        log::error!(
                            "Failed to get children for season '{}': {:?}",
                            item.title, e
                        );
                    }
                }
            }
            "movie" | "episode" => {
                let details_result = {
                    let client = client_arc.lock().await;
                    client
                        .get_item_details(server_uri, server_token, &item.rating_key)
                        .await
                };

                match details_result {
                    Ok(Some(details)) => {
                        if let Some(media) = details.media.first() {
                            if let Some(part) = media.parts.first() {
                                if let Err(e) = db::upsert_media_part(
                                    tx,
                                    part,
                                    &item.rating_key,
                                    server_id,
                                    media,
                                    sync_time,
                                )
                                .await
                                {
                                    log::error!(
                                        "Failed to upsert media part for item '{}': {:?}",
                                        item.title, e
                                    );
                                } else {
                                    for stream in &part.streams {
                                        if let Err(e) =
                                            db::upsert_stream(tx, stream, part.id, server_id, sync_time)
                                                .await
                                        {
                                            log::error!(
                                                "Failed to upsert stream for item '{}': {:?}",
                                                item.title, e
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Ok(None) => log::warn!("No details found for item '{}'", item.title),
                    Err(e) => log::error!("Failed to get details for item '{}': {:?}", item.title, e),
                }
            }
            _ => {}
        }
    })
}

async fn run_database_sync(app_state: &web::Data<AppState>) {
    let sync_start_time = chrono::Utc::now();
    let client_arc = Arc::clone(&app_state.plex_client);
    let db_pool = app_state.db_pool.clone();
    {
        let mut client = client_arc.lock().await;
        if let Err(e) = client.ensure_logged_in().await {
            log::error!("Failed to log in to Plex: {:?}", e);
            return;
        }
    }
    let servers_result = {
        let client = client_arc.lock().await;
        client.get_servers().await
    };
    if let Ok(servers) = servers_result {
        log::info!("Found {} servers. Starting full sync...", servers.len());
        for server in &servers {
            let remote_conn = server.connections.iter().find(|c| !c.local);
            let server_token = server.access_token.as_ref();

            if let (Some(conn), Some(token)) = (remote_conn, server_token) {
                if let Ok(mut tx) = db_pool.begin().await {
                    if db::upsert_server(&mut tx, server, true, sync_start_time)
                        .await
                        .is_err()
                    {
                        let _ = tx.rollback().await;
                        continue;
                    }
                    let _ = tx.commit().await;
                } else {
                    continue;
                }

                let libraries_result = {
                    let client = client_arc.lock().await;
                    client.get_libraries(&conn.uri, token).await
                };
                match libraries_result {
                    Ok(library_list) => {
                        log::info!("Syncing server: {}", server.name);
                        for library in &library_list.libraries {
                            if SUPPORTED_LIBRARY_TYPES.contains(&library.library_type.as_str()) {
                                let mut tx = match db_pool.begin().await {
                                    Ok(tx) => tx,
                                    Err(_) => continue,
                                };
                                log::info!("Syncing library: {}", library.title);
                                if db::upsert_library(
                                    &mut tx,
                                    library,
                                    &server.client_identifier,
                                    sync_start_time,
                                )
                                .await
                                .is_err()
                                {
                                    continue;
                                }

                                let items_result = {
                                    let client = client_arc.lock().await;
                                    client
                                        .get_library_items(&conn.uri, token, &library.key)
                                        .await
                                };

                                if let Ok(item_list) = items_result {
                                    log::info!(
                                        "Found {} items in library '{}'",
                                        item_list.items.len(),
                                        library.title
                                    );
                                    for item in &item_list.items {
                                        sync_item_and_children(
                                            &client_arc,
                                            &mut tx,
                                            &conn.uri,
                                            token,
                                            &server.client_identifier,
                                            &library.key,
                                            item,
                                            sync_start_time,
                                        )
                                        .await;
                                    }
                                } else {
                                    log::error!(
                                        "FAILED to get items for library '{}'",
                                        library.title
                                    );
                                }
                                if tx.commit().await.is_err() {
                                    log::error!(
                                        "FAILED to commit transaction for library '{}'",
                                        library.title
                                    );
                                } else {
                                    log::info!(
                                        "Successfully committed transaction for library '{}'",
                                        library.title
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        log::error!(
                            "Server '{}' is offline. Marking as such. Error: {:?}",
                            server.name, e
                        );
                        if let Ok(mut tx) = db_pool.begin().await {
                            if db::upsert_server(&mut tx, server, false, sync_start_time)
                                .await
                                .is_ok()
                            {
                                let _ = tx.commit().await;
                            }
                        }
                    }
                }
            } else {
                log::warn!(
                    "Skipping server '{}' (no remote connection or token).",
                    server.name
                );
                if let Ok(mut tx) = db_pool.begin().await {
                    if db::upsert_server(&mut tx, server, false, sync_start_time)
                        .await
                        .is_ok()
                    {
                        let _ = tx.commit().await;
                    }
                }
            }
        }
    } else {
        log::error!("Failed to get initial server list.");
    }

    log::info!("Sync loop finished. Pruning old data...");
    if let Err(e) = db::prune_old_data(&db_pool, sync_start_time).await {
        log::error!("Failed to prune old data: {:?}", e);
    }
}

async fn database_sync_scheduler(app_state: web::Data<AppState>) {
    log::info!("Performing initial database sync on startup...");
    run_database_sync(&app_state).await;
    log::info!("Initial sync complete. Starting scheduled runs.");

    let mut interval = tokio::time::interval(Duration::from_secs(60 * 60 * SYNC_INTERVAL_HOURS));
    interval.tick().await;

    loop {
        interval.tick().await;
        log::info!("Starting scheduled database sync...");
        run_database_sync(&app_state).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    env_logger::init();

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let db_pool = PgPoolOptions::new()
        .connect(&database_url)
        .await
        .expect("Failed to connect to Postgres.");
    log::info!("Successfully connected to database.");

    let plex_client = Arc::new(Mutex::new(PlexClient::new()));
    let app_state = web::Data::new(AppState {
        plex_client: plex_client.clone(),
        db_pool: db_pool.clone(),
    });
    tokio::spawn(database_sync_scheduler(app_state.clone()));

    log::info!("Backend server starting on http://0.0.0.0:3001");

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone())
            .configure(routes::configure)
    })
    .bind(("0.0.0.0", 3001))?
    .run()
    .await
}