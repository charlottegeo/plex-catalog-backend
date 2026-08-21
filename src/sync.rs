//! Catalog sync: probe servers, ingest libraries/items, fulfill requests, prune stale rows.

use crate::db;
use crate::models::{Item, MediaRequest};
use crate::plex_client::PlexClient;
use crate::resolution;
use crate::server_status;
use crate::AppState;
use actix_web::web;
use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use std::result::Result as StdResult;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};

#[derive(Clone)]
struct FulfilledPingPart {
    title: String,
    item_type: String,
    season: Option<i32>,
    resolution_display: String,
    server_names: Vec<String>,
}

/// One ping per subscriber per media "guid".
// If multiple seasons are fulfilled in the same sync run, they are batched into a single ping.
fn build_fulfilled_batch_body(parts: &[FulfilledPingPart]) -> String {
    if parts.is_empty() {
        return String::new();
    }
    let title = &parts[0].title;
    let item_type = parts[0].item_type.as_str();
    let is_show = item_type == "show";

    let mut merged_servers: Vec<String> = parts
        .iter()
        .flat_map(|p| p.server_names.iter().cloned())
        .collect();
    merged_servers.sort();
    merged_servers.dedup();
    let server_list = if merged_servers.is_empty() {
        "unknown servers".to_string()
    } else {
        merged_servers.join(", ")
    };

    let mut unique_seasons: Vec<i32> = parts.iter().filter_map(|p| p.season).collect();
    unique_seasons.sort_unstable();
    unique_seasons.dedup();

    let title_fragment = if is_show {
        if unique_seasons.len() > 1 {
            format!(
                "{} (Seasons {})",
                title,
                unique_seasons
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else if unique_seasons.len() == 1 {
            format!("{} (Season {})", title, unique_seasons[0])
        } else {
            title.clone()
        }
    } else {
        title.clone()
    };

    let res_parts: Vec<&str> = parts
        .iter()
        .map(|p| p.resolution_display.as_str())
        .collect();
    let resolution_line = if res_parts.windows(2).all(|w| w[0] == w[1]) && !res_parts.is_empty() {
        res_parts[0].to_string()
    } else {
        res_parts.join(" / ")
    };

    format!(
        "Your request for {} was fulfilled with resolution {} and is now available on: {}.",
        title_fragment, resolution_line, server_list
    )
}

const SUPPORTED_LIBRARY_TYPES: &[&str] = &["movie", "show"];
const DISCOVERY_WORKER_COUNT: usize = 2;
const DETAIL_WORKER_COUNT: usize = 2;

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
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
#[allow(clippy::large_enum_variant)]
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
                        "Batch flush failed after retries count={} attempts={} error={:?}",
                        count,
                        MAX_ATTEMPTS,
                        e
                    );
                    return Err(e);
                }

                let backoff = Duration::from_secs(2u64.pow(attempts));
                tracing::warn!(
                    "Batch upsert failed attempt={}/{} retry_in={:?} error={:?}",
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
#[allow(clippy::too_many_arguments)]
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
            "Extras upsert failed count={} parent={} error={:?}",
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
                && !part.streams.is_empty()
            {
                stream::iter(&part.streams)
                    .for_each_concurrent(2, |stream| {
                        let db_p = db_pool.clone();
                        let part_id = part.id;
                        let s_id = server_id.to_string();
                        async move {
                            if let Err(e) =
                                db::upsert_stream(&db_p, stream, part_id, &s_id, sync_time).await
                            {
                                tracing::error!(
                                    "Stream upsert failed part={} error={:?}",
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
            tracing::error!("Sync semaphore closed error={:?}", e);
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
                    std::slice::from_ref(&item.rating_key),
                    &server_id,
                    sync_time,
                )
                .await
                {
                    tracing::error!("Touch tree failed item={} error={:?}", item.title, e);
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
                    "Parent flush failed item={} type={} error={:?}",
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
                                    "Season flush failed show={} error={:?}",
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
                        tracing::warn!("Seasons fetch failed show={}", item.title);
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
                                    "Episode flush failed show={} error={:?}",
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
                                tracing::error!("Episode detail queue send failed");
                            }
                        }
                    } else {
                        tracing::warn!("Episodes fetch failed show={}", item.title);
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
                                tracing::error!("Detail queue send failed");
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
                            tracing::error!("Detail queue send failed");
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
            tracing::error!("Sync semaphore closed error={:?}", e);
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
                                        .await
                                        {
                                            tracing::error!(
                                                "Stream upsert failed part={} error={:?}",
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
                    "Item details missing type={} key={}",
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

async fn mark_server_unavailable(
    db_pool: &sqlx::PgPool,
    server: &crate::models::Device,
    status: i16,
    connection_uri: &str,
    sync_start_time: chrono::DateTime<chrono::Utc>,
) {
    let _ = db::upsert_server(db_pool, server, status, connection_uri, sync_start_time).await;
}

fn remote_connections_to_try(server: &crate::models::Device) -> Vec<&crate::models::Connection> {
    let mut remotes: Vec<&crate::models::Connection> =
        server.connections.iter().filter(|c| !c.local).collect();
    remotes.sort_by_key(|c| {
        let https = c
            .protocol
            .as_deref()
            .map(|p| p.eq_ignore_ascii_case("https"))
            .unwrap_or_else(|| c.uri.starts_with("https://"));
        match (c.relay, https, c.ipv6) {
            (false, true, false) => 0,
            (false, true, true) => 1,
            (false, false, false) => 2,
            (false, false, true) => 3,
            (true, true, _) => 4,
            (true, false, _) => 5,
        }
    });
    remotes
}

async fn sync_server(
    state: web::Data<AppState>,
    server: crate::models::Device,
    client: PlexClient,
    db_pool: sqlx::PgPool,
    sync_start_time: chrono::DateTime<chrono::Utc>,
) {
    let Some(token) = server.access_token.as_ref() else {
        tracing::warn!(
            server = %server.name,
            status = server_status::NO_CONNECTION,
            reason = "no_token",
            "Server probe failed"
        );
        mark_server_unavailable(
            &db_pool,
            &server,
            server_status::NO_CONNECTION,
            "",
            sync_start_time,
        )
        .await;
        return;
    };

    let remotes = remote_connections_to_try(&server);
    if remotes.is_empty() {
        tracing::warn!(
            server = %server.name,
            status = server_status::NO_CONNECTION,
            reason = "no_remote_uri",
            "Server probe failed"
        );
        mark_server_unavailable(
            &db_pool,
            &server,
            server_status::NO_CONNECTION,
            "",
            sync_start_time,
        )
        .await;
        return;
    }

    // Try every remote URI until libraries load (first success wins).
    let mut library_list = None;
    let mut working_uri = remotes[0].uri.as_str();
    let mut last_status = server_status::UNREACHABLE;

    for conn in &remotes {
        match client.get_libraries(&conn.uri, token).await {
            Ok(list) => {
                library_list = Some(list);
                working_uri = conn.uri.as_str();
                tracing::debug!(
                    server = %server.name,
                    uri = %conn.uri,
                    relay = conn.relay,
                    "Remote probe ok"
                );
                break;
            }
            Err(e) => {
                last_status = server_status::classify_reqwest_error(&e);
                tracing::debug!(
                    server = %server.name,
                    uri = %conn.uri,
                    relay = conn.relay,
                    status = last_status,
                    reason = server_status::reason_label(last_status),
                    error = %e,
                    "Remote probe failed"
                );
            }
        }
    }

    let Some(library_list) = library_list else {
        tracing::warn!(
            server = %server.name,
            status = last_status,
            reason = server_status::reason_label(last_status),
            tried = remotes.len(),
            "Server probe failed"
        );
        mark_server_unavailable(
            &db_pool,
            &server,
            last_status,
            remotes[0].uri.as_str(),
            sync_start_time,
        )
        .await;
        return;
    };

    tracing::info!(server = %server.name, uri = %working_uri, "Server sync start");
    let _ = db::upsert_server(
        &db_pool,
        &server,
        server_status::ONLINE,
        working_uri,
        sync_start_time,
    )
    .await;

    for library in &library_list.libraries {
        if SUPPORTED_LIBRARY_TYPES.contains(&library.library_type.as_str()) {
            tracing::info!(
                library = %library.title,
                server = %server.name,
                "Library sync start"
            );

            let _ = db::upsert_library(
                &db_pool,
                library,
                &server.client_identifier,
                sync_start_time,
            )
            .await;

            let existing_timestamps =
                db::get_library_item_timestamps(&db_pool, &server.client_identifier, &library.key)
                    .await
                    .unwrap_or_default();

            if let Ok(item_list) = client
                .get_library_items(working_uri, token, &library.key)
                .await
            {
                tracing::info!(
                    library = %library.title,
                    count = item_list.items.len(),
                    "Library items"
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
                            .is_none_or(|&old_ts| ts > old_ts),
                        None => true,
                    };

                    if force_scan {
                        if discovery_sender
                            .send(DiscoveryTask::SyncItem {
                                item,
                                server_uri: working_uri.to_string(),
                                server_token: token.to_string(),
                                server_id: server.client_identifier.clone(),
                                library_key: library.key.clone(),
                                sync_time: sync_start_time,
                                force_scan: true,
                            })
                            .await
                            .is_err()
                        {
                            tracing::error!("Discovery queue send failed!");
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
                        tracing::error!(error = ?e, "Batch touch failed!");
                    } else {
                        tracing::info!(
                            library = %library.title,
                            skipped = skipped_count,
                            "Unchanged items touched"
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

                        tracing::debug!(
                            library = %library.title,
                            pct = progress_percent,
                            done = completed,
                            total = total_discovered,
                            rate = format!("{throughput:.1}"),
                            "Library sync progress"
                        );

                        last_progress_log = Instant::now();
                    }

                    if pending == 0 {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        if pending_work.load(Ordering::Relaxed) == 0 {
                            tracing::info!(
                                library = %library.title,
                                duration = ?progress_start.elapsed(),
                                "Library sync done!"
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

                if let Err(e) = flush_item_buffer(item_buffer, &db_pool, sync_start_time).await {
                    tracing::error!(
                        library = %library.title,
                        error = ?e,
                        "Final buffer flush failed"
                    );
                }
            } else {
                tracing::error!(
                    library = %library.title,
                    "Library items fetch failed"
                );
            }
        }
    }
    tracing::info!(server = %server.name, "Server sync done!");
}

async fn run_database_sync(app_state: &web::Data<AppState>) {
    let start_instant = Instant::now();
    let sync_start_time = chrono::Utc::now();
    let client = app_state.plex_client.clone();
    let db_pool = app_state.db_pool.clone();

    tracing::info!("Sync run start at={}", sync_start_time);

    if let Err(e) = db::set_sync_metadata(&db_pool, true, None).await {
        tracing::error!("Set sync_in_progress failed error={:?}", e);
    }

    if let Err(e) = client.ensure_logged_in().await {
        tracing::error!("Plex login failed; sync aborted error={:?}", e);
        let _ = db::set_sync_metadata(&db_pool, false, Some(&format!("Plex login failed: {}", e)))
            .await;
        return;
    }

    if let Ok(servers) = client.get_servers().await {
        tracing::info!("Servers to sync count={}", servers.len());
        let seen_server_ids: Vec<String> = servers
            .iter()
            .map(|s| s.client_identifier.clone())
            .collect();
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

        tracing::info!("Sync ingest done; checking Plex before prune");
        if let Err(e) = client.check_connectivity().await {
            tracing::error!(
                "Plex unreachable before prune; aborting prune error={:?}",
                e
            );
            if let Err(db_err) = db::set_sync_metadata(
                &db_pool,
                false,
                Some("Plex API unreachable - prune aborted to preserve local data"),
            )
            .await
            {
                tracing::error!("sync_metadata update failed error={:?}", db_err);
            }
            return;
        }

        tracing::info!("Pruning stale rows");
        if let Err(e) = db::prune_old_data(&db_pool, sync_start_time, &seen_server_ids).await {
            tracing::error!("Pruning failed error={:?}", e);
        }
    } else {
        tracing::error!("Plex server list fetch failed");
        let _ = db::set_sync_metadata(
            &db_pool,
            false,
            Some("Failed to retrieve server list from Plex API"),
        )
        .await;
        return;
    }

    tracing::info!("Checking media requests");

    if let Ok(expired) = db::get_expired_requests(&db_pool).await {
        for er in &expired {
            for sub in &er.subscribers {
                if let Err(e) = app_state
                    .ping_client
                    .send_expired_ping(sub, &er.title)
                    .await
                {
                    tracing::error!(
                        "Expired ping failed user={} title={} error={:?}",
                        sub,
                        er.title,
                        e
                    );
                }
            }
        }
        if let Ok(deleted) = db::delete_expired_requests(&db_pool).await {
            if deleted > 0 {
                tracing::info!("Deleted expired requests count={}", deleted);
            }
        }
    } else {
        tracing::warn!("Expired requests fetch failed!");
    }

    if let Ok(pending) = db::get_pending_requests(&db_pool).await {
        if pending.is_empty() {
            tracing::info!("No pending requests.");
        } else {
            tracing::info!("Checking pending requests count={}", pending.len());
        }

        let mut fulfilled_for_pings: Vec<(MediaRequest, String, Vec<String>)> = Vec::new();

        for req in pending {
            let fulfilled_resolution = match req.item_type.as_str() {
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
                    db::is_show_request_fulfilled(&db_pool, &req.guid, req.requested_season).await
                }
                _ => Ok(None),
            };

            if let Ok(Some(actual_res)) = fulfilled_resolution {
                if let Err(e) = db::mark_request_fulfilled(&db_pool, req.id).await {
                    tracing::warn!("Mark fulfilled failed id={} error={:?}", req.id, e);
                } else {
                    tracing::info!(
                        "Request fulfilled title={} id={} subscribers={:?}",
                        req.title,
                        req.id,
                        req.subscribers
                    );
                    let resolution = if let Some(req_res) = req
                        .requested_resolution
                        .as_deref()
                        .filter(|r| !r.trim().is_empty())
                    {
                        if resolution::is_resolution_lower(&actual_res, req_res) {
                            format!(
                                "{} (You requested {} - submit an upgrade request if you still want this resolution.)",
                                actual_res, req_res
                            )
                        } else {
                            actual_res
                        }
                    } else {
                        actual_res
                    };
                    let server_names = db::get_server_names_for_guid(&db_pool, &req.guid)
                        .await
                        .unwrap_or_default();
                    fulfilled_for_pings.push((req, resolution, server_names));
                }
            }
        }

        let mut ping_groups: HashMap<(String, String), Vec<FulfilledPingPart>> = HashMap::new();
        for (req, resolution_display, server_names) in fulfilled_for_pings {
            let guid_key = db::media_guid_key(&req.guid);
            for subscriber in &req.subscribers {
                ping_groups
                    .entry((subscriber.clone(), guid_key.clone()))
                    .or_default()
                    .push(FulfilledPingPart {
                        title: req.title.clone(),
                        item_type: req.item_type.clone(),
                        season: req.requested_season,
                        resolution_display: resolution_display.clone(),
                        server_names: server_names.clone(),
                    });
            }
        }

        for ((subscriber, _guid), mut parts) in ping_groups {
            parts.sort_by_key(|p| p.season.unwrap_or(i32::MAX));
            parts.dedup_by(|a, b| {
                a.season == b.season
                    && a.title == b.title
                    && a.resolution_display == b.resolution_display
            });
            let body = build_fulfilled_batch_body(&parts);
            if let Err(e) = app_state
                .ping_client
                .send_fulfilled_ping_body(&subscriber, &body)
                .await
            {
                tracing::error!(
                    "Fulfilled ping failed for user={} error={:?}",
                    subscriber,
                    e
                );
            }
        }
    } else {
        tracing::warn!("Pending requests fetch failed");
    }

    if let Err(e) = db::update_sync_metadata_last_updated(&db_pool).await {
        tracing::error!("sync_metadata update failed error={:?}", e);
    }

    let duration = start_instant.elapsed();
    tracing::info!(
        secs = duration.as_secs(),
        ms = duration.subsec_millis(),
        "Sync run finished!"
    );
}

pub async fn database_sync_scheduler(app_state: web::Data<AppState>) {
    tracing::info!("Initial sync starting...");
    run_database_sync(&app_state).await;

    let mut interval =
        tokio::time::interval(Duration::from_secs(60 * app_state.sync_interval_minutes));

    interval.tick().await;

    loop {
        tracing::info!(
            minutes = app_state.sync_interval_minutes,
            "Next sync scheduled"
        );
        interval.tick().await;
        run_database_sync(&app_state).await;
    }
}
