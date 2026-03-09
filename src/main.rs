use crate::models::{CachedImage, Item};
use crate::plex_client::PlexClient;
use actix_web::{web, App, HttpServer};
use futures::stream::{self, StreamExt};
use moka::future::Cache;
use sqlx::postgres::PgPoolOptions;
use std::result::Result as StdResult;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Semaphore};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

mod auth;
mod db;
mod error;
mod models;
mod plex_client;
mod routes;

/// Adds Bearer (JWT) security scheme so Swagger UI can use the Authorize button for CSH authentication.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let bearer = SecurityScheme::Http(
            HttpBuilder::new()
                .scheme(HttpAuthScheme::Bearer)
                .bearer_format("JWT")
                .description(Some(
                    "Keycloak JWT token (CSHAuth). Use the token from your SSO login.",
                ))
                .build(),
        );
        if let Some(components) = openapi.components.as_mut() {
            components
                .security_schemes
                .insert("bearer_auth".to_string(), bearer);
        }
        openapi.security = Some(vec![utoipa::openapi::SecurityRequirement::new(
            "bearer_auth",
            Vec::<String>::new(),
        )]);
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        routes::get_servers_handler,
        routes::get_system_info_handler,
        routes::get_libraries_handler,
        routes::get_library_items_handler,
        routes::get_item_details_handler,
        routes::get_item_children_handler,
        routes::get_item_extras_handler,
        routes::get_image_handler,
        routes::search_handler,
        routes::discover_handler,
        routes::create_request_handler,
        routes::get_all_requests_handler,
        routes::delete_request_handler,
        routes::get_media_details_handler,
        routes::get_seasons_handler,
        routes::get_episodes_handler,
        routes::create_play_queue_handler,
    ),
    components(schemas(
        crate::models::DbServer,
        crate::models::SystemInfo,
        crate::models::Library,
        crate::models::Item,
        crate::models::ItemWithDetails,
        crate::models::ItemList,
        crate::models::Media,
        crate::models::Part,
        crate::models::Stream,
        crate::models::PlexExtra,
        crate::models::SearchResult,
        crate::models::SearchQuery,
        crate::models::MediaRequestPayload,
        crate::models::MediaRequest,
        crate::models::ImageQuery,
        crate::models::MediaDetails,
        crate::models::MediaVersion,
        crate::models::ServerAvailability,
        crate::models::SeasonSummary,
        crate::models::EpisodeDetails,
        crate::models::PlayQueueResponse,
    )),
    modifiers(&SecurityAddon),
    info(
        title = "Plex Catalog Backend API",
        version = "1.0.0",
        description = "API for browsing a media catalog populated from Plex servers"
    )
)]
struct ApiDoc;

pub const SYNC_INTERVAL_MINUTES: u64 = 90;
const SUPPORTED_LIBRARY_TYPES: &[&str] = &["movie", "show"];
const DISCOVERY_WORKER_COUNT: usize = 2;
const DETAIL_WORKER_COUNT: usize = 2;

#[derive(Clone)]
pub struct AppState {
    pub plex_client: PlexClient,
    pub db_pool: sqlx::PgPool,
    pub image_cache: Cache<String, CachedImage>,
    pub sync_semaphore: Arc<Semaphore>,
}

#[derive(Debug, Clone)]
enum DiscoveryTask {
    SyncItem {
        item: Item,
        server_uri: String,
        server_token: String,
        server_id: String,
        library_key: String,
        sync_time: chrono::DateTime<chrono::Utc>,
        force_scan: bool,
    },
    Shutdown,
}

#[derive(Debug, Clone)]
enum DetailTask {
    SyncItemDetails {
        item: Item,
        server_uri: String,
        server_token: String,
        server_id: String,
        sync_time: chrono::DateTime<chrono::Utc>,
    },
    Shutdown,
}

/// Bulk inserts items into the database using PostgreSQL UNNEST for efficiency.
async fn flush_item_buffer(
    buffer: Arc<Mutex<Vec<db::ItemWithContext>>>,
    db_pool: &sqlx::PgPool,
    sync_time: chrono::DateTime<chrono::Utc>,
) -> StdResult<usize, sqlx::Error> {
    let items = {
        let mut buf = buffer.lock().await;
        if buf.is_empty() {
            return Ok(0);
        }
        buf.drain(..).collect::<Vec<_>>()
    };

    let count = items.len();
    let mut attempts = 0;
    const MAX_ATTEMPTS: u32 = 3;

    while attempts < MAX_ATTEMPTS {
        match db::upsert_items_batch(db_pool, &items, sync_time).await {
            Ok(_) => {
                return Ok(count);
            }
            Err(e) => {
                attempts += 1;
                if attempts >= MAX_ATTEMPTS {
                    tracing::error!(
                        "Critical Database Error: Failed to flush batch of {} items after {} attempts: {:?}",
                        count,
                        MAX_ATTEMPTS,
                        e
                    );
                    return Err(e);
                }

                let backoff = Duration::from_secs(2u64.pow(attempts));
                tracing::warn!(
                    "Database Warning: Batch upsert failed (attempt {}/{}). Retrying in {:?}... Error: {:?}",
                    attempts,
                    MAX_ATTEMPTS,
                    backoff,
                    e
                );
                tokio::time::sleep(backoff).await;
            }
        }
    }

    Ok(count)
}

/// Fetches extras from Plex for a parent item and upserts them into the database.
/// Also fetches media details for each extra.
async fn sync_extras_for_parent(
    plex_client: &crate::plex_client::PlexClient,
    db_pool: &sqlx::PgPool,
    server_uri: &str,
    server_token: &str,
    parent_rating_key: &str,
    library_id: &str,
    server_id: &str,
    sync_time: chrono::DateTime<chrono::Utc>,
) {
    let Ok(extras) = plex_client
        .get_item_extras(server_uri, server_token, parent_rating_key)
        .await
    else {
        return;
    };
    if extras.is_empty() {
        return;
    }
    let items: Vec<db::ItemWithContext> = extras
        .into_iter()
        .enumerate()
        .map(|(idx, e)| {
            let rating_key = e
                .rating_key
                .clone()
                .or_else(|| e.key.rsplit('/').find(|s| !s.is_empty()).map(String::from))
                .unwrap_or_else(|| format!("extra-{}-{}-{}", server_id, parent_rating_key, idx));
            let item = Item {
                guid: Some(format!("local-{}-{}", server_id, rating_key)),
                rating_key: rating_key.clone(),
                parent_id: Some(parent_rating_key.to_string()),
                index: None,
                leaf_count: None,
                title: e.title,
                key: e.key,
                summary: None,
                item_type: "extra".to_string(),
                year: 0,
                media: vec![],
                thumb: e.thumb.clone(),
                art: None,
                updated_at: None,
                content_rating: None,
                duration: None,
                originally_available_at: None,
                studio: None,
                extra_type: e.extra_type,
            };
            db::ItemWithContext {
                item,
                library_id: library_id.to_string(),
                server_id: server_id.to_string(),
            }
        })
        .collect();
    if let Err(e) = db::upsert_items_batch(db_pool, &items, sync_time).await {
        tracing::warn!(
            "Sync Warning: Failed to upsert {} extras for parent {}: {:?}",
            items.len(),
            parent_rating_key,
            e
        );
        return;
    }
    stream::iter(items)
        .for_each_concurrent(2, |ctx| {
            let p_client = plex_client.clone();
            let s_uri = server_uri.to_string();
            let s_token = server_token.to_string();
            let s_id = server_id.to_string();
            let pool = db_pool.clone();
            async move {
                if let Ok(Some(details)) = p_client
                    .get_item_details(&s_uri, &s_token, &ctx.item.rating_key)
                    .await
                {
                    let item_with_media = Item {
                        media: details.media,
                        ..ctx.item
                    };
                    process_media_parts(&item_with_media, &s_id, sync_time, &pool).await;
                }
            }
        })
        .await;
}

/// Upserts media parts and streams for an item.
async fn process_media_parts(
    item: &Item,
    server_id: &str,
    sync_time: chrono::DateTime<chrono::Utc>,
    db_pool: &sqlx::PgPool,
) -> bool {
    let mut at_least_one_part_with_streams = false;
    for media in &item.media {
        for part in &media.parts {
            if db::upsert_media_part(db_pool, part, &item.rating_key, server_id, media, sync_time)
                .await
                .is_ok()
            {
                if !part.streams.is_empty() {
                    stream::iter(&part.streams)
                        .for_each_concurrent(2, |stream| {
                            let db_p = db_pool.clone();
                            let part_id = part.id;
                            let s_id = server_id.to_string();
                            async move {
                                if let Err(e) =
                                    db::upsert_stream(&db_p, stream, part_id, &s_id, sync_time)
                                        .await
                                {
                                    tracing::error!(
                                        "Database Error: Failed to upsert stream for part {}: {:?}",
                                        part_id,
                                        e
                                    );
                                }
                            }
                        })
                        .await;
                    at_least_one_part_with_streams = true;
                }
            }
        }
    }
    at_least_one_part_with_streams
}

/// Processes a discovery task, either syncing a single item or shutting down.
/// This only syncs one at a time so it doesn't use excessive resources.
async fn process_discovery_task(
    task: DiscoveryTask,
    state: &AppState,
    _discovery_sender: &mpsc::Sender<DiscoveryTask>,
    detail_sender: &mpsc::Sender<DetailTask>,
    item_buffer: Arc<Mutex<Vec<db::ItemWithContext>>>,
    pending_work: Arc<AtomicU64>,
    total_tasks_created: Arc<AtomicU64>,
) {
    let _permit = match state.sync_semaphore.acquire().await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Critical: Sync semaphore closed unexpectedly: {:?}", e);
            pending_work.fetch_sub(1, Ordering::Relaxed);
            return;
        }
    };

    match task {
        DiscoveryTask::SyncItem {
            item,
            server_uri,
            server_token,
            server_id,
            library_key,
            sync_time,
            force_scan,
        } => {
            if !force_scan {
                {
                    let mut buf = item_buffer.lock().await;
                    buf.push(db::ItemWithContext {
                        item: item.clone(),
                        library_id: library_key.clone(),
                        server_id: server_id.clone(),
                    });
                }

                if let Err(e) = db::touch_items_batch(
                    &state.db_pool,
                    &[item.rating_key.clone()],
                    &server_id,
                    sync_time,
                )
                .await
                {
                    tracing::error!(
                        "Database Error: Failed to touch tree for item '{}': {:?}",
                        item.title,
                        e
                    );
                }

                pending_work.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            {
                let mut buf = item_buffer.lock().await;
                buf.push(db::ItemWithContext {
                    item: item.clone(),
                    library_id: library_key.clone(),
                    server_id: server_id.clone(),
                });
                drop(buf);
            }

            if let Err(e) = flush_item_buffer(item_buffer.clone(), &state.db_pool, sync_time).await
            {
                tracing::error!(
                    "Database Error: Failed to flush parent item '{}' ({}). Aborting children processing to prevent FK violations: {:?}",
                    item.title,
                    item.item_type,
                    e
                );
                pending_work.fetch_sub(1, Ordering::Relaxed);
                return;
            }

            match item.item_type.as_str() {
                "show" => {
                    drop(_permit);
                    tokio::time::sleep(Duration::from_millis(100)).await;

                    let children_result = state
                        .plex_client
                        .get_item_children(&server_uri, &server_token, &item.rating_key)
                        .await;

                    if let Ok(children) = children_result {
                        if !children.items.is_empty() {
                            let seasons_count = children.items.len() as u64;
                            pending_work.fetch_add(seasons_count, Ordering::Relaxed);
                            total_tasks_created.fetch_add(seasons_count, Ordering::Relaxed);

                            let mut seasons_to_batch = Vec::new();
                            for child in children.items {
                                let mut child_to_sync = child.clone();
                                if child_to_sync.parent_id.is_none() {
                                    child_to_sync.parent_id = Some(item.rating_key.clone());
                                }
                                seasons_to_batch.push(db::ItemWithContext {
                                    item: child_to_sync,
                                    library_id: library_key.clone(),
                                    server_id: server_id.clone(),
                                });
                            }

                            let season_rating_keys: Vec<_> = seasons_to_batch
                                .iter()
                                .map(|ctx| ctx.item.rating_key.clone())
                                .collect();
                            {
                                let mut buf = item_buffer.lock().await;
                                buf.extend(seasons_to_batch);
                                drop(buf);
                            }

                            if let Err(e) =
                                flush_item_buffer(item_buffer.clone(), &state.db_pool, sync_time)
                                    .await
                            {
                                tracing::error!(
                                    "Database Error: Failed to flush seasons for show '{}'. Aborting episode processing: {:?}",
                                    item.title,
                                    e
                                );
                                pending_work.fetch_sub(seasons_count, Ordering::Relaxed);
                                pending_work.fetch_sub(1, Ordering::Relaxed);
                                return;
                            }
                            sync_extras_for_parent(
                                &state.plex_client,
                                &state.db_pool,
                                &server_uri,
                                &server_token,
                                &item.rating_key,
                                &library_key,
                                &server_id,
                                sync_time,
                            )
                            .await;
                            for rating_key in &season_rating_keys {
                                sync_extras_for_parent(
                                    &state.plex_client,
                                    &state.db_pool,
                                    &server_uri,
                                    &server_token,
                                    rating_key,
                                    &library_key,
                                    &server_id,
                                    sync_time,
                                )
                                .await;
                            }
                            pending_work.fetch_sub(seasons_count, Ordering::Relaxed);
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    } else {
                        tracing::warn!(
                            "Sync Warning: Could not fetch seasons for show '{}'",
                            item.title
                        );
                    }

                    let leaves_result = state
                        .plex_client
                        .get_item_all_leaves(&server_uri, &server_token, &item.rating_key)
                        .await;

                    if let Ok(leaves) = leaves_result {
                        let episodes_count = leaves.items.len() as u64;
                        pending_work.fetch_add(episodes_count, Ordering::Relaxed);
                        total_tasks_created.fetch_add(episodes_count, Ordering::Relaxed);

                        let mut episodes_to_batch = Vec::new();
                        let mut episodes_needing_details = Vec::new();

                        for leaf_item in leaves.items {
                            let mut episode_item = leaf_item.clone();
                            if leaf_item.item_type == "episode" && episode_item.parent_id.is_none()
                            {
                                episode_item.parent_id = Some(item.rating_key.clone());
                            }

                            let needs_detail_fetch = episode_item.media.is_empty()
                                || episode_item
                                    .media
                                    .iter()
                                    .flat_map(|m| &m.parts)
                                    .all(|p| p.streams.is_empty());

                            episodes_to_batch.push(db::ItemWithContext {
                                item: episode_item.clone(),
                                library_id: library_key.clone(),
                                server_id: server_id.clone(),
                            });

                            if needs_detail_fetch {
                                episodes_needing_details.push(episode_item);
                            }
                        }

                        if !episodes_to_batch.is_empty() {
                            let episode_rating_keys: Vec<_> = episodes_to_batch
                                .iter()
                                .map(|ep| ep.item.rating_key.clone())
                                .collect();
                            {
                                let mut buf = item_buffer.lock().await;
                                buf.extend(episodes_to_batch);
                                drop(buf);
                            }

                            if let Err(e) =
                                flush_item_buffer(item_buffer.clone(), &state.db_pool, sync_time)
                                    .await
                            {
                                tracing::error!(
                                    "Database Error: Failed to flush episode batch for show '{}': {:?}",
                                    item.title,
                                    e
                                );
                                pending_work.fetch_sub(episodes_count, Ordering::Relaxed);
                            } else {
                                pending_work.fetch_sub(episodes_count, Ordering::Relaxed);
                                for rating_key in &episode_rating_keys {
                                    sync_extras_for_parent(
                                        &state.plex_client,
                                        &state.db_pool,
                                        &server_uri,
                                        &server_token,
                                        rating_key,
                                        &library_key,
                                        &server_id,
                                        sync_time,
                                    )
                                    .await;
                                }
                            }
                        }

                        for episode_item in episodes_needing_details {
                            pending_work.fetch_add(1, Ordering::Relaxed);
                            total_tasks_created.fetch_add(1, Ordering::Relaxed);
                            if detail_sender
                                .send(DetailTask::SyncItemDetails {
                                    item: episode_item,
                                    server_uri: server_uri.clone(),
                                    server_token: server_token.clone(),
                                    server_id: server_id.clone(),
                                    sync_time,
                                })
                                .await
                                .is_err()
                            {
                                pending_work.fetch_sub(1, Ordering::Relaxed);
                                total_tasks_created.fetch_sub(1, Ordering::Relaxed);
                                tracing::error!("Failed to send episode detail task to queue");
                            }
                        }
                    } else {
                        tracing::warn!(
                            "Sync Warning: Could not fetch episodes for show '{}'",
                            item.title
                        );
                    }
                }
                "season" => {
                    drop(_permit);
                    sync_extras_for_parent(
                        &state.plex_client,
                        &state.db_pool,
                        &server_uri,
                        &server_token,
                        &item.rating_key,
                        &library_key,
                        &server_id,
                        sync_time,
                    )
                    .await;
                }
                "movie" | "episode" => {
                    sync_extras_for_parent(
                        &state.plex_client,
                        &state.db_pool,
                        &server_uri,
                        &server_token,
                        &item.rating_key,
                        &library_key,
                        &server_id,
                        sync_time,
                    )
                    .await;
                    if !item.media.is_empty() {
                        let streams_processed =
                            process_media_parts(&item, &server_id, sync_time, &state.db_pool).await;
                        if !streams_processed {
                            pending_work.fetch_add(1, Ordering::Relaxed);
                            total_tasks_created.fetch_add(1, Ordering::Relaxed);
                            if detail_sender
                                .send(DetailTask::SyncItemDetails {
                                    item,
                                    server_uri: server_uri.clone(),
                                    server_token: server_token.clone(),
                                    server_id: server_id.clone(),
                                    sync_time,
                                })
                                .await
                                .is_err()
                            {
                                pending_work.fetch_sub(1, Ordering::Relaxed);
                                total_tasks_created.fetch_sub(1, Ordering::Relaxed);
                                tracing::error!("Failed to send details task to detail queue");
                            }
                        }
                    } else {
                        pending_work.fetch_add(1, Ordering::Relaxed);
                        total_tasks_created.fetch_add(1, Ordering::Relaxed);
                        if detail_sender
                            .send(DetailTask::SyncItemDetails {
                                item,
                                server_uri: server_uri.clone(),
                                server_token: server_token.clone(),
                                server_id,
                                sync_time,
                            })
                            .await
                            .is_err()
                        {
                            pending_work.fetch_sub(1, Ordering::Relaxed);
                            total_tasks_created.fetch_sub(1, Ordering::Relaxed);
                            tracing::error!("Failed to send details task to detail queue");
                        }
                    }
                }
                _ => {
                    drop(_permit);
                }
            }
        }
        DiscoveryTask::Shutdown => {}
    }

    pending_work.fetch_sub(1, Ordering::Relaxed);
}

async fn process_detail_task(
    task: DetailTask,
    state: &AppState,
    pending_work: Arc<AtomicU64>,
    _total_tasks_created: Arc<AtomicU64>,
) {
    let _permit = match state.sync_semaphore.acquire().await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Critical: Sync semaphore closed unexpectedly: {:?}", e);
            pending_work.fetch_sub(1, Ordering::Relaxed);
            return;
        }
    };

    match task {
        DetailTask::SyncItemDetails {
            item,
            server_uri,
            server_token,
            server_id,
            sync_time,
        } => {
            if let Ok(Some(details)) = state
                .plex_client
                .get_item_details(&server_uri, &server_token, &item.rating_key)
                .await
            {
                for media in &details.media {
                    for part in &media.parts {
                        if db::upsert_media_part(
                            &state.db_pool,
                            part,
                            &item.rating_key,
                            &server_id,
                            media,
                            sync_time,
                        )
                        .await
                        .is_ok()
                        {
                            let part_id = part.id;
                            stream::iter(&part.streams)
                                .for_each_concurrent(2, |stream| {
                                    let db_p = state.db_pool.clone();
                                    let s_id = server_id.clone();
                                    async move {
                                        if let Err(e) = db::upsert_stream(
                                            &db_p, stream, part_id, &s_id, sync_time,
                                        )
                                        .await {
                                            tracing::error!(
                                                "Database Error: Failed to upsert stream for part {}: {:?}",
                                                part_id,
                                                e
                                            );
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
        DetailTask::Shutdown => {}
    }

    pending_work.fetch_sub(1, Ordering::Relaxed);
}

async fn discovery_worker(
    discovery_receiver: Arc<Mutex<mpsc::Receiver<DiscoveryTask>>>,
    discovery_sender: mpsc::Sender<DiscoveryTask>,
    detail_sender: mpsc::Sender<DetailTask>,
    state: Arc<AppState>,
    item_buffer: Arc<Mutex<Vec<db::ItemWithContext>>>,
    pending_work: Arc<AtomicU64>,
    total_tasks_created: Arc<AtomicU64>,
) {
    loop {
        let task = {
            let mut receiver = discovery_receiver.lock().await;
            receiver.recv().await
        };

        match task {
            Some(DiscoveryTask::Shutdown) => {
                break;
            }
            Some(task) => {
                process_discovery_task(
                    task,
                    &state,
                    &discovery_sender,
                    &detail_sender,
                    item_buffer.clone(),
                    pending_work.clone(),
                    total_tasks_created.clone(),
                )
                .await;
            }
            None => {
                if pending_work.load(Ordering::Relaxed) == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

async fn detail_worker(
    detail_receiver: Arc<Mutex<mpsc::Receiver<DetailTask>>>,
    state: Arc<AppState>,
    pending_work: Arc<AtomicU64>,
    total_tasks_created: Arc<AtomicU64>,
) {
    loop {
        let task = {
            let mut receiver = detail_receiver.lock().await;
            receiver.recv().await
        };

        match task {
            Some(DetailTask::Shutdown) => {
                break;
            }
            Some(task) => {
                process_detail_task(
                    task,
                    &state,
                    pending_work.clone(),
                    total_tasks_created.clone(),
                )
                .await;
            }
            None => {
                if pending_work.load(Ordering::Relaxed) == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
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

                    let existing_timestamps = db::get_library_item_timestamps(
                        &db_pool,
                        &server.client_identifier,
                        &library.key,
                    )
                    .await
                    .unwrap_or_default();

                    if let Ok(item_list) = client
                        .get_library_items(&conn.uri, token, &library.key)
                        .await
                    {
                        tracing::info!(
                            "Processing {} items from library '{}'",
                            item_list.items.len(),
                            library.title
                        );

                        let (discovery_sender, discovery_receiver) = mpsc::channel(1000);
                        let (detail_sender, detail_receiver) = mpsc::channel(1000);
                        let discovery_receiver = Arc::new(Mutex::new(discovery_receiver));
                        let detail_receiver = Arc::new(Mutex::new(detail_receiver));
                        let item_buffer = Arc::new(Mutex::new(Vec::<db::ItemWithContext>::new()));
                        let pending_work = Arc::new(AtomicU64::new(0));
                        let total_tasks_created = Arc::new(AtomicU64::new(0));
                        let state_clone = state.clone();

                        let mut discovery_handles = Vec::new();
                        for _ in 0..DISCOVERY_WORKER_COUNT {
                            let receiver = discovery_receiver.clone();
                            let d_sender = discovery_sender.clone();
                            let d_detail_sender = detail_sender.clone();
                            let worker_state = state_clone.clone();
                            let worker_buffer = item_buffer.clone();
                            let worker_pending = pending_work.clone();
                            let worker_total = total_tasks_created.clone();
                            discovery_handles.push(tokio::spawn(async move {
                                discovery_worker(
                                    receiver,
                                    d_sender,
                                    d_detail_sender,
                                    worker_state.into_inner(),
                                    worker_buffer,
                                    worker_pending,
                                    worker_total,
                                )
                                .await;
                            }));
                        }

                        let mut detail_handles = Vec::new();
                        for _ in 0..DETAIL_WORKER_COUNT {
                            let receiver = detail_receiver.clone();
                            let worker_state = state_clone.clone();
                            let worker_pending = pending_work.clone();
                            let worker_total = total_tasks_created.clone();
                            detail_handles.push(tokio::spawn(async move {
                                detail_worker(
                                    receiver,
                                    worker_state.into_inner(),
                                    worker_pending,
                                    worker_total,
                                )
                                .await;
                            }));
                        }

                        let total_base_items = item_list.items.len() as u64;
                        pending_work.store(total_base_items, Ordering::Relaxed);
                        total_tasks_created.store(total_base_items, Ordering::Relaxed);

                        let mut items_to_touch = Vec::new();

                        for item in item_list.items {
                            let force_scan = match item.updated_at {
                                Some(ts) => existing_timestamps
                                    .get(&item.rating_key)
                                    .map_or(true, |&old_ts| ts > old_ts),
                                None => true,
                            };

                            if force_scan {
                                if discovery_sender
                                    .send(DiscoveryTask::SyncItem {
                                        item,
                                        server_uri: conn.uri.clone(),
                                        server_token: token.to_string(),
                                        server_id: server.client_identifier.clone(),
                                        library_key: library.key.clone(),
                                        sync_time: sync_start_time,
                                        force_scan: true,
                                    })
                                    .await
                                    .is_err()
                                {
                                    tracing::error!(
                                        "Failed to send initial task to discovery queue"
                                    );
                                    pending_work.fetch_sub(1, Ordering::Relaxed);
                                    total_tasks_created.fetch_sub(1, Ordering::Relaxed);
                                }
                            } else {
                                items_to_touch.push(item.rating_key);
                                pending_work.fetch_sub(1, Ordering::Relaxed);
                            }
                        }

                        if !items_to_touch.is_empty() {
                            let skipped_count = items_to_touch.len();
                            if let Err(e) = db::touch_items_batch(
                                &db_pool,
                                &items_to_touch,
                                &server.client_identifier,
                                sync_start_time,
                            )
                            .await
                            {
                                tracing::error!("Database Error: Batch touch failed: {:?}", e);
                            } else {
                                tracing::info!(
                                    "[{}] Skipped and updated {} unchanged items.",
                                    library.title,
                                    skipped_count
                                );
                            }
                        }

                        let progress_start = Instant::now();
                        let mut last_progress_log = Instant::now();

                        loop {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            let pending = pending_work.load(Ordering::Relaxed);
                            let total_discovered = total_tasks_created.load(Ordering::Relaxed);

                            if last_progress_log.elapsed() >= Duration::from_secs(5) {
                                let completed = total_discovered.saturating_sub(pending);
                                let progress_percent = if total_discovered > 0 {
                                    (completed * 100) / total_discovered
                                } else {
                                    100
                                };

                                let elapsed = progress_start.elapsed().as_secs_f64();
                                let throughput = if elapsed > 0.0 {
                                    completed as f64 / elapsed
                                } else {
                                    0.0
                                };

                                tracing::info!(
                                    "[{}] Progress: {}% | {}/{} items synced | Rate: {:.1} items/sec",
                                    library.title,
                                    progress_percent,
                                    completed,
                                    total_discovered,
                                    throughput
                                );

                                last_progress_log = Instant::now();
                            }

                            if pending == 0 {
                                tokio::time::sleep(Duration::from_millis(200)).await;
                                if pending_work.load(Ordering::Relaxed) == 0 {
                                    tracing::info!(
                                        "[{}] Sync Complete! Duration: {:?}",
                                        library.title,
                                        progress_start.elapsed()
                                    );
                                    break;
                                }
                            }
                        }

                        for _ in 0..DISCOVERY_WORKER_COUNT {
                            let _ = discovery_sender.send(DiscoveryTask::Shutdown).await;
                        }

                        for _ in 0..DETAIL_WORKER_COUNT {
                            let _ = detail_sender.send(DetailTask::Shutdown).await;
                        }

                        for handle in discovery_handles {
                            let _ = handle.await;
                        }
                        for handle in detail_handles {
                            let _ = handle.await;
                        }

                        if let Err(e) =
                            flush_item_buffer(item_buffer, &db_pool, sync_start_time).await
                        {
                            tracing::error!(
                                "Database Error: Failed to flush final item buffer for library '{}': {:?}",
                                library.title,
                                e
                            );
                        }
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
            tracing::warn!(
                "Connection Warning: Server '{}' is unreachable. Marking as offline.",
                server.name
            );
            let _ = db::upsert_server(&db_pool, &server, false, sync_start_time).await;
        }
    } else {
        tracing::warn!(
            "Skip: Server '{}' has no valid remote connection. Marking as offline.",
            server.name
        );
        let _ = db::upsert_server(&db_pool, &server, false, sync_start_time).await;
    }
}

async fn run_database_sync(app_state: &web::Data<AppState>) {
    let start_instant = Instant::now();
    let sync_start_time = chrono::Utc::now();
    let client = app_state.plex_client.clone();
    let db_pool = app_state.db_pool.clone();

    tracing::info!("Initiating database sync run at {}", sync_start_time);

    if let Err(e) = db::set_sync_metadata(&db_pool, true, None).await {
        tracing::error!("Database Error: Failed to set sync_in_progress: {:?}", e);
    }

    if let Err(e) = client.ensure_logged_in().await {
        tracing::error!("Auth Error: Failed Plex login. Sync aborted: {:?}", e);
        let _ = db::set_sync_metadata(&db_pool, false, Some(&format!("Plex login failed: {}", e)))
            .await;
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
        let _ = db::set_sync_metadata(
            &db_pool,
            false,
            Some("Failed to retrieve server list from Plex API"),
        )
        .await;
        return;
    }

    // Checks if Plex API is reachable before pruning so it doesn't empty the database if
    // the Plex API is unreachable during a sync.
    tracing::info!("Sync complete. Verifying Plex API connectivity before pruning...");
    if let Err(e) = client.check_connectivity().await {
        tracing::error!(
            "Connectivity Check Failed: Plex API unreachable before prune. Aborting prune to avoid data loss. Error: {:?}",
            e
        );
        if let Err(db_err) = db::set_sync_metadata(
            &db_pool,
            false,
            Some("Plex API Unreachable - prune aborted to preserve local data"),
        )
        .await
        {
            tracing::error!(
                "Database Error: Failed to update sync_metadata: {:?}",
                db_err
            );
        }
        return;
    }

    tracing::info!("Connectivity verified. Pruning orphaned data...");
    if let Err(e) = db::prune_old_data(&db_pool, sync_start_time).await {
        tracing::error!("Database Error: Data pruning failed: {:?}", e);
    }

    if let Ok(deleted) = db::delete_stale_requests(&db_pool).await {
        if deleted > 0 {
            tracing::info!(
                "Deleted {} stale media requests (pending > 30 days)",
                deleted
            );
        }
    } else {
        tracing::warn!("Failed to delete stale media requests");
    }

    if let Ok(pending) = db::get_pending_requests(&db_pool).await {
        for req in pending {
            let fulfilled = match req.item_type.as_str() {
                "movie" => {
                    db::is_movie_request_fulfilled(
                        &db_pool,
                        &req.guid,
                        req.requested_resolution.as_deref(),
                        req.is_upgrade,
                    )
                    .await
                }
                "show" => {
                    db::is_show_request_fulfilled(
                        &db_pool,
                        &req.guid,
                        req.requested_seasons.as_deref(),
                        req.requested_resolution.as_deref(),
                        req.is_upgrade,
                    )
                    .await
                }
                _ => Ok(false),
            };

            if let Ok(true) = fulfilled {
                if let Err(e) = db::mark_request_fulfilled(&db_pool, req.id).await {
                    tracing::warn!("Failed to mark request {} as fulfilled: {:?}", req.id, e);
                } else {
                    tracing::info!(
                        "Media request fulfilled: {} (id={}) for user {}",
                        req.title,
                        req.id,
                        req.username
                    );
                    // TODO: send ping to user
                }
            }
        }
    } else {
        tracing::warn!("Failed to fetch pending media requests");
    }

    if let Err(e) = db::update_sync_metadata_last_updated(&db_pool).await {
        tracing::error!("Database Error: Failed to update sync_metadata: {:?}", e);
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

    let mut interval = tokio::time::interval(Duration::from_secs(60 * SYNC_INTERVAL_MINUTES));

    interval.tick().await;

    loop {
        tracing::info!("Next scheduled sync in {} minutes.", SYNC_INTERVAL_MINUTES);
        interval.tick().await;
        run_database_sync(&app_state).await;
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
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

    sqlx::migrate!("./migrations")
        .run(&db_pool)
        .await
        .expect("Migration failed");
    tracing::info!("Database schema is up to date.");

    let app_state = web::Data::new(AppState {
        plex_client: PlexClient::new(),
        db_pool: db_pool.clone(),
        image_cache: Cache::builder()
            .max_capacity(256 * 1024 * 1024)
            .weigher(|_key, value: &CachedImage| -> u32 { value.bytes.len() as u32 })
            .time_to_live(Duration::from_secs(12 * 60 * 60))
            .build(),
        sync_semaphore: Arc::new(Semaphore::new(5)),
    });

    tokio::spawn(database_sync_scheduler(app_state.clone()));

    tracing::info!("HTTP server starting on 0.0.0.0:3001");

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone())
            .configure(routes::configure)
            .service(
                utoipa_swagger_ui::SwaggerUi::new("/swagger-ui/{_:.*}")
                    .url("/api-docs/openapi.json", ApiDoc::openapi()),
            )
    })
    .workers(2)
    .bind(("0.0.0.0", 3001))?
    .run()
    .await
}
