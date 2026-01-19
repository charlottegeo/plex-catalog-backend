use crate::models::Item;
use crate::plex_client::PlexClient;
use actix_web::{web, App, HttpServer};
use bytes::Bytes;
use futures::stream::{self, StreamExt};
use moka::future::Cache;
use sqlx::postgres::PgPoolOptions;
use std::result::Result as StdResult;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Semaphore};

mod auth;
mod db;
mod error;
mod models;
mod plex_client;
mod routes;

const SYNC_INTERVAL_HOURS: u64 = 12;
const SUPPORTED_LIBRARY_TYPES: &[&str] = &["movie", "show"];
const BATCH_SIZE: usize = 75;
const DISCOVERY_WORKER_COUNT: usize = 2;
const DETAIL_WORKER_COUNT: usize = 2;

#[derive(Clone)]
pub struct AppState {
    pub plex_client: PlexClient,
    pub db_pool: sqlx::PgPool,
    pub image_cache: Cache<String, Bytes>,
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
    let max_attempts = 3;

    while attempts < max_attempts {
        match db::upsert_items_batch(db_pool, &items, sync_time).await {
            Ok(_) => {
                tracing::info!("Successfully batch upserted {} items", count);
                return Ok(count);
            }
            Err(e) => {
                attempts += 1;
                if attempts >= max_attempts {
                    tracing::error!(
                        "Critical Database Error: Failed to flush batch of {} items after {} attempts: {:?}",
                        count,
                        max_attempts,
                        e
                    );
                    return Err(e);
                }

                let backoff = Duration::from_secs(2u64.pow(attempts as u32));
                tracing::warn!(
                    "Database Warning: Batch upsert failed (attempt {}/{}). Retrying in {:?}... Error: {:?}",
                    attempts,
                    max_attempts,
                    backoff,
                    e
                );
                tokio::time::sleep(backoff).await;
            }
        }
    }

    Ok(count)
}

async fn process_media_parts(
    item: &Item,
    server_id: &str,
    sync_time: chrono::DateTime<chrono::Utc>,
    db_pool: &sqlx::PgPool,
) -> bool {
    if let Some(media) = item.media.first() {
        if let Some(part) = media.parts.first() {
            if db::upsert_media_part(db_pool, part, &item.rating_key, server_id, media, sync_time)
                .await
                .is_ok()
            {
                if !part.streams.is_empty() {
                    stream::iter(&part.streams)
                        .for_each_concurrent(2, |stream| {
                            let db_p = db_pool.clone();
                            let s_id = server_id.to_string();
                            async move {
                                if let Err(e) =
                                    db::upsert_stream(&db_p, stream, part.id, &s_id, sync_time)
                                        .await
                                {
                                    tracing::error!(
                                        "Database Error: Failed to upsert stream for part {}: {:?}",
                                        part.id,
                                        e
                                    );
                                }
                            }
                        })
                        .await;
                    return true;
                } else {
                    return false;
                }
            }
        }
    }
    false
}

async fn process_discovery_task(
    task: DiscoveryTask,
    state: &AppState,
    discovery_sender: &mpsc::Sender<DiscoveryTask>,
    detail_sender: &mpsc::Sender<DetailTask>,
    item_buffer: Arc<Mutex<Vec<db::ItemWithContext>>>,
    pending_work: Arc<AtomicU64>,
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
        } => {
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
                                pending_work.fetch_sub(1, Ordering::Relaxed);
                                return;
                            }

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
                            }
                        }

                        for episode_item in episodes_needing_details {
                            pending_work.fetch_add(1, Ordering::Relaxed);
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
                }
                "movie" | "episode" => {
                    if !item.media.is_empty() {
                        let streams_processed =
                            process_media_parts(&item, &server_id, sync_time, &state.db_pool).await;
                        if !streams_processed {
                            pending_work.fetch_add(1, Ordering::Relaxed);
                            if detail_sender
                                .send(DetailTask::SyncItemDetails {
                                    item,
                                    server_uri,
                                    server_token,
                                    server_id,
                                    sync_time,
                                })
                                .await
                                .is_err()
                            {
                                pending_work.fetch_sub(1, Ordering::Relaxed);
                                tracing::error!("Failed to send details task to detail queue");
                            }
                        }
                    } else {
                        pending_work.fetch_add(1, Ordering::Relaxed);
                        if detail_sender
                            .send(DetailTask::SyncItemDetails {
                                item,
                                server_uri,
                                server_token,
                                server_id,
                                sync_time,
                            })
                            .await
                            .is_err()
                        {
                            pending_work.fetch_sub(1, Ordering::Relaxed);
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

async fn process_detail_task(task: DetailTask, state: &AppState, pending_work: Arc<AtomicU64>) {
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
                if let Some(media) = details.media.first() {
                    if let Some(part) = media.parts.first() {
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
                            stream::iter(&part.streams)
                                .for_each_concurrent(2, |stream| {
                                    let db_p = state.db_pool.clone();
                                    let s_id = server_id.clone();
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
                process_detail_task(task, &state, pending_work.clone()).await;
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
                        let state_clone = state.clone();

                        let mut discovery_handles = Vec::new();
                        for _ in 0..DISCOVERY_WORKER_COUNT {
                            let receiver = discovery_receiver.clone();
                            let d_sender = discovery_sender.clone();
                            let d_detail_sender = detail_sender.clone();
                            let worker_state = state_clone.clone();
                            let worker_buffer = item_buffer.clone();
                            let worker_pending = pending_work.clone();
                            discovery_handles.push(tokio::spawn(async move {
                                discovery_worker(
                                    receiver,
                                    d_sender,
                                    d_detail_sender,
                                    worker_state.into_inner(),
                                    worker_buffer,
                                    worker_pending,
                                )
                                .await;
                            }));
                        }

                        let mut detail_handles = Vec::new();
                        for _ in 0..DETAIL_WORKER_COUNT {
                            let receiver = detail_receiver.clone();
                            let worker_state = state_clone.clone();
                            let worker_pending = pending_work.clone();
                            detail_handles.push(tokio::spawn(async move {
                                detail_worker(receiver, worker_state.into_inner(), worker_pending)
                                    .await;
                            }));
                        }

                        let total_base_items = item_list.items.len() as u64;
                        let initial_count = total_base_items;
                        pending_work.store(initial_count, Ordering::Relaxed);

                        for item in item_list.items {
                            if discovery_sender
                                .send(DiscoveryTask::SyncItem {
                                    item,
                                    server_uri: conn.uri.clone(),
                                    server_token: token.to_string(),
                                    server_id: server.client_identifier.clone(),
                                    library_key: library.key.clone(),
                                    sync_time: sync_start_time,
                                })
                                .await
                                .is_err()
                            {
                                tracing::error!("Failed to send initial task to discovery queue");
                                pending_work.fetch_sub(1, Ordering::Relaxed);
                            }
                        }

                        let progress_start = Instant::now();
                        let mut last_progress_log = Instant::now();
                        let mut last_pending_count = initial_count;

                        loop {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            let pending = pending_work.load(Ordering::Relaxed);

                            if last_progress_log.elapsed() >= Duration::from_secs(5) {
                                let completed = total_base_items.saturating_sub(pending);
                                let progress_percent = if total_base_items > 0 {
                                    ((completed * 100) / total_base_items).min(100)
                                } else {
                                    100
                                };

                                let elapsed_secs = progress_start.elapsed().as_secs_f64();
                                let tasks_completed = last_pending_count.saturating_sub(pending);
                                let throughput = if elapsed_secs > 0.0 {
                                    tasks_completed as f64 / elapsed_secs
                                } else {
                                    0.0
                                };

                                tracing::info!(
                                    "[{}] | Progress: {}% | Pending Tasks: {} | Throughput: {:.1} tasks/sec",
                                    library.title,
                                    progress_percent,
                                    pending,
                                    throughput
                                );

                                last_progress_log = Instant::now();
                                last_pending_count = pending;
                            }

                            if pending == 0 {
                                tokio::time::sleep(Duration::from_millis(200)).await;
                                if pending_work.load(Ordering::Relaxed) == 0 {
                                    let elapsed_secs = progress_start.elapsed().as_secs_f64();
                                    let total_completed = total_base_items;
                                    let avg_throughput = if elapsed_secs > 0.0 {
                                        total_completed as f64 / elapsed_secs
                                    } else {
                                        0.0
                                    };
                                    tracing::info!(
                                        "[{}] | Complete: 100% | Total Items: {} | Avg Throughput: {:.1} tasks/sec | Duration: {:.1}s",
                                        library.title,
                                        total_completed,
                                        avg_throughput,
                                        elapsed_secs
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

    let app_state = web::Data::new(AppState {
        plex_client: PlexClient::new(),
        db_pool: db_pool.clone(),
        image_cache: Cache::builder()
            .max_capacity(500 * 1024 * 1024)
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
    })
    .workers(2)
    .bind(("0.0.0.0", 3001))?
    .run()
    .await
}
