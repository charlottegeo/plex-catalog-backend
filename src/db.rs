use crate::models::{
    DbServer, Device, EpisodeDetails, Item, Library, Media, MediaDetails, MediaRequest,
    MediaVersion, Part, SearchResult, SeasonSummary, ServerAvailability, Stream, SystemInfo,
};
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};

#[derive(FromRow)]
struct ItemByGuid {
    id: String,
    server_id: String,
    server_name: String,
    access_token: String,
    guid: Option<String>,
    title: String,
    summary: Option<String>,
    year: Option<i16>,
    originally_available_at: Option<chrono::NaiveDate>,
    art_path: Option<String>,
    thumb_path: Option<String>,
    item_type: String,
    content_rating: Option<String>,
    duration: Option<i64>,
}

#[derive(FromRow)]
struct VersionDetails {
    item_id: Option<String>,
    video_resolution: Option<String>,
    subtitle_language: Option<String>,
}

#[derive(FromRow)]
struct EpisodeWithVersion {
    id: String,
    title: String,
    summary: Option<String>,
    thumb_path: Option<String>,
    index: Option<i32>,
    content_rating: Option<String>,
    duration: Option<i64>,
    video_resolution: Option<String>,
    subtitle_language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ItemWithContext {
    pub item: Item,
    pub library_id: String,
    pub server_id: String,
}

pub async fn get_all_servers(pool: &PgPool) -> Result<Vec<DbServer>, sqlx::Error> {
    sqlx::query_as!(
        DbServer,
        "SELECT * FROM servers ORDER BY is_online DESC, name"
    )
    .fetch_all(pool)
    .await
}

pub async fn get_system_info(
    pool: &PgPool,
    sync_interval_minutes: u64,
) -> Result<SystemInfo, sqlx::Error> {
    use sqlx::Row;
    let row = sqlx::query(
        r#"
        SELECT
            (SELECT last_updated FROM sync_metadata WHERE id = 1) AS last_updated,
            (SELECT COUNT(DISTINCT guid) FROM items WHERE item_type = 'movie' AND guid IS NOT NULL) AS total_movies,
            (SELECT COUNT(DISTINCT guid) FROM items WHERE item_type = 'show' AND guid IS NOT NULL) AS total_shows,
            (SELECT COUNT(*) FROM servers WHERE is_online = TRUE) AS online_servers,
            (SELECT COUNT(*) FROM servers WHERE is_online = FALSE) AS offline_servers,
            (SELECT last_error FROM sync_metadata WHERE id = 1) AS last_error,
            COALESCE((SELECT sync_in_progress FROM sync_metadata WHERE id = 1), false) AS sync_in_progress
        "#,
    )
    .fetch_one(pool)
    .await?;
    let last_updated: Option<DateTime<Utc>> = row.try_get("last_updated").ok().flatten();
    let total_movies: i64 = row.try_get::<i64, _>("total_movies").unwrap_or(0);
    let total_shows: i64 = row.try_get::<i64, _>("total_shows").unwrap_or(0);
    let online_servers: i64 = row.try_get::<i64, _>("online_servers").unwrap_or(0);
    let offline_servers: i64 = row.try_get::<i64, _>("offline_servers").unwrap_or(0);
    let last_error: Option<String> = row.try_get("last_error").ok().flatten();
    let sync_in_progress: bool = row.try_get::<bool, _>("sync_in_progress").unwrap_or(false);
    Ok(SystemInfo {
        last_updated,
        sync_interval_minutes,
        total_movies,
        total_shows,
        online_servers,
        offline_servers,
        last_error,
        sync_in_progress,
    })
}

pub async fn update_sync_metadata_last_updated(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO sync_metadata (id, last_updated, last_error, sync_in_progress)
        VALUES (1, $1, NULL, false)
        ON CONFLICT (id) DO UPDATE SET
            last_updated = EXCLUDED.last_updated,
            last_error = NULL,
            sync_in_progress = false
        "#,
    )
    .bind(Utc::now())
    .execute(pool)
    .await?;
    Ok(())
}

/// Updates the sync metadata in the database with the last error and sync in progress status.
pub async fn set_sync_metadata(
    pool: &PgPool,
    sync_in_progress: bool,
    last_error: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO sync_metadata (id, sync_in_progress, last_error)
        VALUES (1, $1, $2)
        ON CONFLICT (id) DO UPDATE SET
            sync_in_progress = EXCLUDED.sync_in_progress,
            last_error = EXCLUDED.last_error
        "#,
    )
    .bind(sync_in_progress)
    .bind(last_error)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_server_libraries(
    pool: &PgPool,
    server_id: &str,
) -> Result<Vec<Library>, sqlx::Error> {
    sqlx::query(
        "SELECT id, title, library_type FROM libraries WHERE server_id = $1 ORDER BY title ASC",
    )
    .bind(server_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| {
        use sqlx::Row;
        Ok(Library {
            key: row.try_get("id")?,
            title: row.try_get("title")?,
            library_type: row.try_get("library_type")?,
        })
    })
    .collect()
}

pub async fn get_library_items(
    pool: &PgPool,
    server_id: &str,
    library_id: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<Item>, sqlx::Error> {
    sqlx::query(
        r#"
        SELECT
            i.id,
            i.guid,
            i.parent_id,
            i.title,
            i.summary,
            i.item_type,
            i.year,
            i.thumb_path,
            i.art_path,
            i.index,
            i.leaf_count,
            i.updated_at,
            i.content_rating,
            i.duration,
            i.originally_available_at,
            i.studio,
            i.extra_type,
            (SELECT mp.video_resolution
             FROM media_parts mp
             WHERE mp.item_id = i.id AND mp.server_id = i.server_id
             ORDER BY CASE
                 WHEN mp.video_resolution ILIKE '%2160%' OR mp.video_resolution ILIKE '%4k%' THEN 4
                 WHEN mp.video_resolution ILIKE '%1080%' THEN 3
                 WHEN mp.video_resolution ILIKE '%720%' THEN 2
                 WHEN mp.video_resolution ILIKE '%480%' THEN 1
                 ELSE 0
             END DESC
             LIMIT 1) AS best_resolution
        FROM items i
        WHERE i.server_id = $1 AND i.library_id = $2 AND i.item_type IN ('movie', 'show')
        ORDER BY i.title ASC
        LIMIT $3 OFFSET $4
        "#,
    )
    .bind(server_id)
    .bind(library_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| {
        use sqlx::Row;
        let id: String = row.try_get("id")?;
        let best_resolution: Option<String> = row.try_get("best_resolution").ok().flatten();
        let media = best_resolution
            .map(|r| {
                vec![Media {
                    video_resolution: Some(r),
                    parts: vec![],
                }]
            })
            .unwrap_or_default();
        Ok(Item {
            guid: row.try_get("guid")?,
            rating_key: id.clone(),
            key: format!("/library/metadata/{}", id),
            parent_id: row.try_get("parent_id")?,
            title: row.try_get("title")?,
            summary: row.try_get("summary")?,
            item_type: row.try_get("item_type")?,
            year: row.try_get::<i16, _>("year").unwrap_or(0) as u16,
            media,
            thumb: row.try_get("thumb_path")?,
            extra_type: row.try_get("extra_type").ok().flatten(),
            art: row.try_get("art_path")?,
            index: row.try_get("index")?,
            leaf_count: row.try_get("leaf_count")?,
            updated_at: row.try_get("updated_at")?,
            content_rating: row.try_get("content_rating")?,
            duration: row.try_get("duration")?,
            originally_available_at: row
                .try_get::<Option<chrono::NaiveDate>, _>("originally_available_at")
                .ok()
                .flatten(),
            studio: row.try_get("studio")?,
        })
    })
    .collect()
}

pub async fn get_library_items_total(
    pool: &PgPool,
    server_id: &str,
    library_id: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) as "count!"
        FROM items
        WHERE server_id = $1 AND library_id = $2 AND item_type IN ('movie', 'show')
        "#,
        server_id,
        library_id
    )
    .fetch_one(pool)
    .await
}

pub async fn upsert_server(
    pool: &PgPool,
    server: &Device,
    is_online: bool,
    sync_time: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let access_token = server.access_token.as_deref().unwrap_or_default();
    let owner_username = server.source_title.as_deref();
    let connection_uri = server
        .connections
        .iter()
        .find(|c| !c.local)
        .map_or("", |c| &c.uri);

    sqlx::query!(
        r#"
        INSERT INTO servers (id, name, owner_username, access_token, connection_uri, last_seen, is_online)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (id) DO UPDATE SET
            last_seen = EXCLUDED.last_seen,
            is_online = EXCLUDED.is_online,
            name = CASE WHEN servers.name IS DISTINCT FROM EXCLUDED.name THEN EXCLUDED.name ELSE servers.name END,
            owner_username = CASE WHEN servers.owner_username IS DISTINCT FROM EXCLUDED.owner_username THEN EXCLUDED.owner_username ELSE servers.owner_username END,
            access_token = CASE WHEN servers.access_token IS DISTINCT FROM EXCLUDED.access_token THEN EXCLUDED.access_token ELSE servers.access_token END,
            connection_uri = CASE WHEN servers.connection_uri IS DISTINCT FROM EXCLUDED.connection_uri THEN EXCLUDED.connection_uri ELSE servers.connection_uri END
        "#,
        server.client_identifier,
        server.name,
        owner_username,
        access_token,
        connection_uri,
        sync_time,
        is_online
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn upsert_library(
    pool: &PgPool,
    library: &Library,
    server_id: &str,
    sync_time: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO libraries (id, server_id, title, library_type, last_seen)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (id, server_id) DO UPDATE SET
            last_seen = EXCLUDED.last_seen,
            title = CASE WHEN libraries.title IS DISTINCT FROM EXCLUDED.title THEN EXCLUDED.title ELSE libraries.title END,
            library_type = CASE WHEN libraries.library_type IS DISTINCT FROM EXCLUDED.library_type THEN EXCLUDED.library_type ELSE libraries.library_type END
        "#,
        library.key,
        server_id,
        library.title,
        library.library_type,
        sync_time
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Batch upserts items and their media parts.
pub async fn upsert_items_batch(
    pool: &PgPool,
    items: &[ItemWithContext],
    sync_time: DateTime<Utc>,
) -> Result<usize, sqlx::Error> {
    if items.is_empty() {
        return Ok(0);
    }

    let ids: Vec<String> = items.iter().map(|i| i.item.rating_key.clone()).collect();
    let library_ids: Vec<String> = items.iter().map(|i| i.library_id.clone()).collect();
    let server_ids: Vec<String> = items.iter().map(|i| i.server_id.clone()).collect();
    let parent_ids: Vec<Option<String>> = items.iter().map(|i| i.item.parent_id.clone()).collect();
    let titles: Vec<String> = items.iter().map(|i| i.item.title.clone()).collect();
    let summaries: Vec<Option<String>> = items
        .iter()
        .map(|i| i.item.summary.as_ref().filter(|s| !s.is_empty()).cloned())
        .collect();
    let item_types: Vec<String> = items.iter().map(|i| i.item.item_type.clone()).collect();
    let years: Vec<i16> = items.iter().map(|i| i.item.year as i16).collect();
    let thumb_paths: Vec<Option<String>> = items.iter().map(|i| i.item.thumb.clone()).collect();
    let art_paths: Vec<Option<String>> = items.iter().map(|i| i.item.art.clone()).collect();
    let guids: Vec<Option<String>> = items
        .iter()
        .map(|i| match &i.item.guid {
            Some(g) if g.starts_with("local-") => Some(g.clone()),
            Some(g) => Some(g.rsplit('/').next().unwrap_or(g).to_string()),
            None => Some(format!("local-{}-{}", i.server_id, i.item.rating_key)),
        })
        .collect();
    let indices: Vec<Option<i32>> = items.iter().map(|i| i.item.index).collect();
    let leaf_counts: Vec<Option<i32>> = items.iter().map(|i| i.item.leaf_count).collect();
    let updated_ats: Vec<Option<i64>> = items.iter().map(|i| i.item.updated_at).collect();
    let content_ratings: Vec<Option<String>> = items
        .iter()
        .map(|i| i.item.content_rating.clone())
        .collect();
    let durations: Vec<Option<i64>> = items.iter().map(|i| i.item.duration).collect();
    let originally_available_at: Vec<Option<chrono::NaiveDate>> = items
        .iter()
        .map(|i| i.item.originally_available_at)
        .collect();
    let studios: Vec<Option<String>> = items.iter().map(|i| i.item.studio.clone()).collect();
    let extra_types: Vec<Option<String>> =
        items.iter().map(|i| i.item.extra_type.clone()).collect();
    let sync_times: Vec<DateTime<Utc>> = vec![sync_time; items.len()];

    let result = sqlx::query(
        r#"
        INSERT INTO items (id, library_id, server_id, parent_id, title, summary, item_type, year, thumb_path, art_path, last_seen, guid, index, leaf_count, updated_at, content_rating, duration, originally_available_at, studio, extra_type)
        SELECT * FROM UNNEST(
            $1::text[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::text[], $8::smallint[],
            $9::text[], $10::text[], $11::timestamptz[], $12::text[], $13::integer[], $14::integer[], $15::bigint[], $16::text[], $17::bigint[], $18::date[], $19::text[], $20::text[]
        )
        ON CONFLICT (id, server_id) DO UPDATE SET
            last_seen = EXCLUDED.last_seen,
            title = EXCLUDED.title,
            summary = EXCLUDED.summary,
            year = EXCLUDED.year,
            thumb_path = EXCLUDED.thumb_path,
            art_path = EXCLUDED.art_path,
            guid = EXCLUDED.guid,
            index = EXCLUDED.index,
            leaf_count = EXCLUDED.leaf_count,
            updated_at = EXCLUDED.updated_at,
            content_rating = EXCLUDED.content_rating,
            duration = EXCLUDED.duration,
            originally_available_at = EXCLUDED.originally_available_at,
            studio = EXCLUDED.studio,
            extra_type = EXCLUDED.extra_type
        "#,
    )
    .bind(&ids[..])
    .bind(&library_ids[..])
    .bind(&server_ids[..])
    .bind(&parent_ids[..])
    .bind(&titles[..])
    .bind(&summaries[..])
    .bind(&item_types[..])
    .bind(&years[..])
    .bind(&thumb_paths[..])
    .bind(&art_paths[..])
    .bind(&sync_times[..])
    .bind(&guids[..])
    .bind(&indices[..])
    .bind(&leaf_counts[..])
    .bind(&updated_ats[..])
    .bind(&content_ratings[..])
    .bind(&durations[..])
    .bind(&originally_available_at[..])
    .bind(&studios[..])
    .bind(&extra_types[..])
    .execute(pool)
    .await?;

    Ok(result.rows_affected() as usize)
}

pub async fn get_library_item_timestamps(
    pool: &PgPool,
    server_id: &str,
    library_id: &str,
) -> Result<std::collections::HashMap<String, i64>, sqlx::Error> {
    struct TsRow {
        id: String,
        updated_at: Option<i64>,
    }

    let rows = sqlx::query_as!(
        TsRow,
        "SELECT id, updated_at FROM items WHERE server_id = $1 AND library_id = $2 AND updated_at IS NOT NULL",
        server_id,
        library_id
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| (r.id, r.updated_at.unwrap_or(0)))
        .collect())
}

pub async fn touch_items_batch(
    pool: &PgPool,
    item_ids: &[String],
    server_id: &str,
    sync_time: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    if item_ids.is_empty() {
        return Ok(());
    }

    sqlx::query(
        r#"
        UPDATE items 
        SET last_seen = $3
        WHERE server_id = $2 AND (
            id = ANY($1)
            OR parent_id = ANY($1)
            OR parent_id IN (SELECT id FROM items WHERE parent_id = ANY($1) AND server_id = $2)
        )
        "#,
    )
    .bind(item_ids)
    .bind(server_id)
    .bind(sync_time)
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        UPDATE media_parts 
        SET last_seen = $3
        WHERE server_id = $2 AND item_id IN (
            SELECT id FROM items WHERE server_id = $2 AND (
                id = ANY($1)
                OR parent_id = ANY($1)
                OR parent_id IN (SELECT id FROM items WHERE parent_id = ANY($1) AND server_id = $2)
            )
        )
        "#,
    )
    .bind(item_ids)
    .bind(server_id)
    .bind(sync_time)
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        UPDATE streams 
        SET last_seen = $3
        WHERE server_id = $2 AND media_part_id IN (
            SELECT id FROM media_parts WHERE server_id = $2 AND item_id IN (
                SELECT id FROM items WHERE server_id = $2 AND (
                    id = ANY($1)
                    OR parent_id = ANY($1)
                    OR parent_id IN (SELECT id FROM items WHERE parent_id = ANY($1) AND server_id = $2)
                )
            )
        )
        "#,
    )
    .bind(item_ids)
    .bind(server_id)
    .bind(sync_time)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn upsert_media_part(
    pool: &PgPool,
    part: &Part,
    item_id: &str,
    server_id: &str,
    media: &Media,
    sync_time: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let resolution = media.video_resolution.as_deref().unwrap_or("Unknown");
    sqlx::query!(
        r#"
        INSERT INTO media_parts (id, item_id, server_id, video_resolution, last_seen)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (id, server_id) DO UPDATE SET
            last_seen = EXCLUDED.last_seen,
            video_resolution = CASE WHEN media_parts.video_resolution IS DISTINCT FROM EXCLUDED.video_resolution THEN EXCLUDED.video_resolution ELSE media_parts.video_resolution END
        "#,
        part.id,
        item_id,
        server_id,
        resolution,
        sync_time
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn upsert_stream(
    pool: &PgPool,
    stream: &Stream,
    part_id: i64,
    server_id: &str,
    sync_time: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO streams (id, media_part_id, server_id, stream_type, language, language_code, format, last_seen)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (id, server_id) DO UPDATE SET
            last_seen = EXCLUDED.last_seen,
            language = CASE WHEN streams.language IS DISTINCT FROM EXCLUDED.language THEN EXCLUDED.language ELSE streams.language END,
            language_code = CASE WHEN streams.language_code IS DISTINCT FROM EXCLUDED.language_code THEN EXCLUDED.language_code ELSE streams.language_code END,
            format = CASE WHEN streams.format IS DISTINCT FROM EXCLUDED.format THEN EXCLUDED.format ELSE streams.format END
        "#,
        stream.id,
        part_id,
        server_id,
        stream.stream_type as i16,
        stream.language,
        stream.language_code,
        stream.format,
        sync_time
    )
    .execute(pool).await?;
    Ok(())
}

pub async fn prune_old_data(
    pool: &PgPool,
    sync_start_time: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    tracing::debug!("Pruning old data from database...");
    let streams_deleted = sqlx::query!("DELETE FROM streams WHERE last_seen < $1", sync_start_time)
        .execute(pool)
        .await?
        .rows_affected();
    let parts_deleted = sqlx::query!(
        "DELETE FROM media_parts WHERE last_seen < $1",
        sync_start_time
    )
    .execute(pool)
    .await?
    .rows_affected();
    let items_deleted = sqlx::query!("DELETE FROM items WHERE last_seen < $1", sync_start_time)
        .execute(pool)
        .await?
        .rows_affected();
    let libraries_deleted = sqlx::query!(
        "DELETE FROM libraries WHERE last_seen < $1",
        sync_start_time
    )
    .execute(pool)
    .await?
    .rows_affected();
    let servers_deleted = sqlx::query!("DELETE FROM servers WHERE last_seen < $1", sync_start_time)
        .execute(pool)
        .await?
        .rows_affected();
    tracing::info!(
        "Pruning complete. Pruned: {} servers, {} libraries, {} items, {} parts, {} streams.",
        servers_deleted,
        libraries_deleted,
        items_deleted,
        parts_deleted,
        streams_deleted
    );
    Ok(())
}

pub async fn search_items(
    pool: &PgPool,
    query: &str,
    limit: i64,
    offset: i64,
) -> Result<Vec<SearchResult>, sqlx::Error> {
    let fts_query = query
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect::<Vec<String>>()
        .join(" & ")
        + ":*";
    sqlx::query_as!(
        SearchResult,
        r#"
        SELECT
            i.id as "rating_key!",
            i.guid,
            i.title, i.summary, i.item_type, i.year, i.thumb_path,
            i.content_rating,
            i.duration,
            i.originally_available_at,
            s.id as "server_id!",
            s.name as "server_name!",
            ts_rank(i.fts_document, to_tsquery('simple', $1)) as rank
        FROM items i
        JOIN servers s ON i.server_id = s.id
        WHERE
            i.fts_document @@ to_tsquery('simple', $1)
            AND i.item_type IN ('movie', 'show')
        ORDER BY rank DESC
        LIMIT $2 OFFSET $3
        "#,
        fts_query,
        limit,
        offset
    )
    .fetch_all(pool)
    .await
}

pub async fn search_items_total(pool: &PgPool, query: &str) -> Result<i64, sqlx::Error> {
    let fts_query = query
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect::<Vec<String>>()
        .join(" & ")
        + ":*";

    sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) as "count!"
        FROM items i
        WHERE
            i.fts_document @@ to_tsquery('simple', $1)
            AND i.item_type IN ('movie', 'show')
        "#,
        fts_query
    )
    .fetch_one(pool)
    .await
}

pub async fn get_server_details(
    pool: &PgPool,
    server_id: &str,
) -> Result<Option<DbServer>, sqlx::Error> {
    sqlx::query_as!(DbServer, "SELECT * FROM servers WHERE id = $1", server_id)
        .fetch_optional(pool)
        .await
}

pub async fn get_details_by_guid(
    pool: &PgPool,
    guid: &str,
) -> Result<Option<MediaDetails>, sqlx::Error> {
    let normalized = guid_for_item_lookup(guid);
    let items = sqlx::query_as!(
        ItemByGuid,
        r#"
        SELECT i.id, i.server_id, s.name as "server_name!", s.access_token as "access_token!", i.guid as "guid?", i.title, i.summary, i.year, i.originally_available_at, i.art_path, i.thumb_path, i.item_type, i.content_rating, i.duration
        FROM items i
        JOIN servers s ON i.server_id = s.id
        WHERE i.guid = $1
        "#,
        normalized
    )
    .fetch_all(pool)
    .await?;

    if items.is_empty() {
        return Ok(None);
    }

    let first_item = &items[0];
    let item_ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();

    let versions = sqlx::query_as!(
        VersionDetails,
        r#"
        SELECT
            mp.item_id,
            mp.video_resolution as "video_resolution?",
            s.language as "subtitle_language?"
        FROM media_parts mp
        LEFT JOIN streams s ON s.media_part_id = mp.id AND s.server_id = mp.server_id AND s.stream_type = 3
        WHERE mp.item_id = ANY($1)
        "#,
        &item_ids
    )
    .fetch_all(pool)
    .await?;

    let mut available_on: Vec<ServerAvailability> = Vec::new();
    for item in &items {
        let mut server_versions: Vec<MediaVersion> = Vec::new();
        let mut resolution_to_subtitles: std::collections::HashMap<
            String,
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();

        for version in versions
            .iter()
            .filter(|v| v.item_id.as_ref() == Some(&item.id))
        {
            if let Some(resolution) = &version.video_resolution {
                let subtitles = resolution_to_subtitles
                    .entry(resolution.clone())
                    .or_default();
                if let Some(subtitle) = &version.subtitle_language {
                    subtitles.insert(subtitle.clone());
                }
            }
        }

        for (resolution, subtitles) in resolution_to_subtitles {
            server_versions.push(MediaVersion {
                video_resolution: resolution,
                subtitles: subtitles.into_iter().collect(),
            });
        }
        available_on.push(ServerAvailability {
            server_id: item.server_id.clone(),
            server_name: item.server_name.clone(),
            rating_key: item.id.clone(),
            access_token: item.access_token.clone(),
            versions: server_versions,
        });
    }

    Ok(Some(MediaDetails {
        guid: first_item.guid.clone().unwrap_or_default(),
        title: first_item.title.clone(),
        summary: first_item.summary.clone(),
        year: first_item.year,
        originally_available_at: first_item.originally_available_at,
        art_path: first_item.art_path.clone(),
        thumb_path: first_item.thumb_path.clone(),
        item_type: first_item.item_type.clone(),
        content_rating: first_item.content_rating.clone(),
        duration: first_item.duration,
        available_on,
    }))
}

pub async fn get_show_seasons(
    pool: &PgPool,
    show_id: &str,
    server_id: &str,
) -> Result<Vec<SeasonSummary>, sqlx::Error> {
    let seasons = sqlx::query_as!(
        SeasonSummary,
        r#"
        SELECT
            id, title, summary as "summary?", thumb_path, art_path, leaf_count
        FROM items
        WHERE parent_id = $1 AND server_id = $2 AND item_type = 'season'
        ORDER BY "index" ASC, title ASC
        "#,
        show_id,
        server_id
    )
    .fetch_all(pool)
    .await?;

    if seasons.is_empty() {
        let has_direct_episodes = sqlx::query!(
            "SELECT 1 as exists FROM items WHERE parent_id = $1 AND server_id = $2 AND item_type = 'episode' LIMIT 1",
            show_id,
            server_id
        )
        .fetch_optional(pool)
        .await?
        .is_some();

        if has_direct_episodes {
            let show_as_season = sqlx::query_as!(
                SeasonSummary,
                r#"
                SELECT
                    id, title, summary as "summary?", thumb_path, art_path, leaf_count
                FROM items
                WHERE id = $1 AND server_id = $2 AND item_type = 'show'
                "#,
                show_id,
                server_id
            )
            .fetch_optional(pool)
            .await?;

            if let Some(show) = show_as_season {
                return Ok(vec![show]);
            }
        }
    }

    Ok(seasons)
}

pub async fn get_season_episodes(
    pool: &PgPool,
    season_id: &str,
    server_id: &str,
) -> Result<Vec<EpisodeDetails>, sqlx::Error> {
    let rows = sqlx::query_as!(
        EpisodeWithVersion,
        r#"
        SELECT
            e.id, 
            e.title, 
            e.summary as "summary?", 
            e.thumb_path, 
            e.index,
            e.content_rating,
            e.duration,
            mp.video_resolution, 
            s.language as "subtitle_language"
        FROM items e
        LEFT JOIN media_parts mp ON mp.item_id = e.id AND mp.server_id = e.server_id
        LEFT JOIN streams s ON s.media_part_id = mp.id AND s.server_id = mp.server_id AND s.stream_type = 3
        WHERE e.server_id = $2 
          AND e.item_type = 'episode'
          AND (
            e.parent_id = $1
            OR (
                e.parent_id = (SELECT parent_id FROM items WHERE id = $1 AND server_id = $2 AND item_type = 'season')
                AND EXISTS (SELECT 1 FROM items WHERE id = $1 AND server_id = $2 AND item_type = 'season')
            )
            OR e.id = $1
          )
        ORDER BY e.index ASC
        "#,
        season_id,
        server_id
    )
    .fetch_all(pool)
    .await?;

    let mut episodes: std::collections::HashMap<String, EpisodeDetails> =
        std::collections::HashMap::new();

    for row in rows {
        let episode = episodes
            .entry(row.id.clone())
            .or_insert_with(|| EpisodeDetails {
                id: row.id.clone(),
                title: row.title.clone(),
                summary: row.summary.clone(),
                thumb_path: row.thumb_path.clone(),
                index: row.index,
                content_rating: row.content_rating.clone(),
                duration: row.duration,
                versions: Vec::new(),
            });

        if let Some(resolution) = row.video_resolution {
            if let Some(version) = episode
                .versions
                .iter_mut()
                .find(|v| v.video_resolution == resolution)
            {
                if let Some(lang) = row.subtitle_language {
                    if !version.subtitles.contains(&lang) {
                        version.subtitles.push(lang);
                    }
                }
            } else {
                episode.versions.push(MediaVersion {
                    video_resolution: resolution,
                    subtitles: row.subtitle_language.map_or(vec![], |l| vec![l]),
                });
            }
        }
    }

    let mut sorted_episodes: Vec<EpisodeDetails> = episodes.into_values().collect();
    sorted_episodes.sort_by_key(|e| e.index.unwrap_or(0));

    Ok(sorted_episodes)
}

/// Check if an item exists in the catalog using its guid.
pub async fn item_exists_by_guid(pool: &PgPool, guid: &str) -> Result<bool, sqlx::Error> {
    let normalized = guid_for_item_lookup(guid);
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM items WHERE guid = $1) AS "exists!""#,
        normalized
    )
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

pub async fn get_request_by_id(
    pool: &PgPool,
    id: i32,
) -> Result<Option<MediaRequest>, sqlx::Error> {
    sqlx::query_as!(
        MediaRequest,
        r#"
        SELECT
            mr.id, mr.guid, mr.title, mr.item_type, mr.requested_season,
            mr.requested_resolution, mr.status, mr.created_at, mr.updated_at,
            mr.is_upgrade, mr.thumb, mr.year, mr.duration, mr.is_viewed,
            ARRAY[]::TEXT[] as "server_names: Vec<String>",
            (
                SELECT COALESCE(array_agg(DISTINCT mrs.username), ARRAY[]::TEXT[])
                FROM media_request_subscribers mrs
                WHERE mrs.request_id = mr.id
            ) as "subscribers!: Vec<String>"
        FROM media_requests mr
        WHERE mr.id = $1
        "#,
        id
    )
    .fetch_optional(pool)
    .await
}

/// Create a new request or subscribe to an existing one.
/// Returns (Request, is_new) to determine if server owners should be pinged.
pub async fn create_or_subscribe_request(
    pool: &PgPool,
    username: &str,
    payload: &crate::models::MediaRequestPayload,
    season: Option<i32>,
    is_upgrade: bool,
) -> Result<(MediaRequest, bool), sqlx::Error> {
    let mut tx = pool.begin().await?;

    let existing_id: Option<i32> = sqlx::query_scalar!(
        r#"
        SELECT id FROM media_requests 
        WHERE status = 'pending' AND guid = $1 AND item_type = $2 
          AND requested_season IS NOT DISTINCT FROM $3 
          AND requested_resolution IS NOT DISTINCT FROM $4
        "#,
        payload.guid,
        payload.item_type,
        season,
        payload.requested_resolution
    )
    .fetch_optional(&mut *tx)
    .await?;

    let (request_id, is_new) = if let Some(id) = existing_id {
        (id, false)
    } else {
        let new_id = sqlx::query_scalar!(
            r#"
            INSERT INTO media_requests (guid, title, item_type, requested_season, requested_resolution, status, is_upgrade, thumb, is_viewed, year, duration)
            VALUES ($1, $2, $3, $4, $5, 'pending', $6, $7, false, $8, $9)
            RETURNING id
            "#,
            payload.guid, payload.title, payload.item_type, season, payload.requested_resolution, is_upgrade, payload.thumb, payload.year, payload.duration
        ).fetch_one(&mut *tx).await?;
        (new_id, true)
    };

    sqlx::query!(
        "INSERT INTO media_request_subscribers (request_id, username) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        request_id, username
    ).execute(&mut *tx).await?;

    tx.commit().await?;

    let req = get_request_by_id(pool, request_id)
        .await?
        .expect("media_requests row must exist after subscribe");
    Ok((req, is_new))
}

/// Unsubscribe a user from a request. Deletes the parent request if no subscribers are left.
pub async fn unsubscribe_request(
    pool: &PgPool,
    request_id: i32,
    username: &str,
) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let res = sqlx::query!(
        "DELETE FROM media_request_subscribers WHERE request_id = $1 AND username = $2",
        request_id,
        username
    )
    .execute(&mut *tx)
    .await?;

    let rows_affected = res.rows_affected();
    if rows_affected > 0 {
        let count = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM media_request_subscribers WHERE request_id = $1",
            request_id
        )
        .fetch_one(&mut *tx)
        .await?
        .unwrap_or(0);

        if count == 0 {
            sqlx::query!("DELETE FROM media_requests WHERE id = $1", request_id)
                .execute(&mut *tx)
                .await?;
        }
    }

    tx.commit().await?;
    Ok(rows_affected)
}

/// Fetch all pending media requests.
pub async fn get_pending_requests(pool: &PgPool) -> Result<Vec<MediaRequest>, sqlx::Error> {
    sqlx::query_as!(
        MediaRequest,
        r#"
        SELECT id, guid, title, item_type, requested_season, requested_resolution, is_upgrade, thumb, is_viewed, year, duration, ARRAY[]::TEXT[] as "server_names: Vec<String>", status, created_at, updated_at,
        (SELECT COALESCE(array_agg(DISTINCT mrs.username), ARRAY[]::TEXT[]) FROM media_request_subscribers mrs WHERE mrs.request_id = media_requests.id) as "subscribers!: Vec<String>"
        FROM media_requests WHERE status = 'pending' ORDER BY created_at ASC
        "#
    ).fetch_all(pool).await
}

/// Fetch active (pending) media requests for a specific guid.
pub async fn get_active_requests_by_guid(
    pool: &PgPool,
    guid: &str,
) -> Result<Vec<MediaRequest>, sqlx::Error> {
    let normalized = guid_for_item_lookup(guid);
    sqlx::query_as!(
        MediaRequest,
        r#"
        SELECT id, guid, title, item_type, requested_season, requested_resolution, is_upgrade, thumb, is_viewed, year, duration, ARRAY[]::TEXT[] as "server_names: Vec<String>", status, created_at, updated_at,
        (SELECT COALESCE(array_agg(DISTINCT mrs.username), ARRAY[]::TEXT[]) FROM media_request_subscribers mrs WHERE mrs.request_id = media_requests.id) as "subscribers!: Vec<String>"
        FROM media_requests WHERE status = 'pending' AND guid = $1 ORDER BY created_at ASC
        "#,
        normalized
    ).fetch_all(pool).await
}

/// Fetch all media requests (both pending and fulfilled).
pub async fn get_all_requests(pool: &PgPool) -> Result<Vec<MediaRequest>, sqlx::Error> {
    sqlx::query_as!(
        MediaRequest,
        r#"
        SELECT mr.id, mr.guid, mr.title, mr.item_type, mr.requested_season, mr.requested_resolution, mr.status, mr.created_at, mr.updated_at, mr.is_upgrade, mr.thumb, mr.year, mr.duration, mr.is_viewed,
        (SELECT COALESCE(array_agg(DISTINCT s.name), ARRAY[]::TEXT[]) FROM items i JOIN servers s ON i.server_id = s.id WHERE i.guid = mr.guid) as "server_names: Vec<String>",
        (SELECT COALESCE(array_agg(DISTINCT mrs.username), ARRAY[]::TEXT[]) FROM media_request_subscribers mrs WHERE mrs.request_id = mr.id) as "subscribers!: Vec<String>"
        FROM media_requests mr ORDER BY CASE WHEN mr.status = 'pending' THEN 0 ELSE 1 END, mr.created_at DESC
        "#
    ).fetch_all(pool).await
}

/// Mark a media request as fulfilled.
pub async fn mark_request_fulfilled(pool: &PgPool, id: i32) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE media_requests SET status = 'fulfilled', updated_at = NOW() WHERE id = $1",
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete expired/outdated requests: pending > 30 days by created_at, fulfilled > 30 days by updated_at.
pub async fn delete_expired_requests(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let result: sqlx::postgres::PgQueryResult = sqlx::query!(
        r#"
        DELETE FROM media_requests
        WHERE (status = 'pending' AND created_at < NOW() - INTERVAL '30 days')
           OR (status = 'fulfilled' AND updated_at < NOW() - INTERVAL '30 days')
        "#
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Get server names that have an item with the given guid.
pub async fn get_server_names_for_guid(
    pool: &PgPool,
    guid: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let normalized = guid_for_item_lookup(guid);
    let rows = sqlx::query_scalar!(
        "SELECT DISTINCT s.name FROM items i JOIN servers s ON i.server_id = s.id WHERE i.guid = $1",
        normalized
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Expired request info for pings before deletion.
pub struct ExpiredRequest {
    pub title: String,
    pub subscribers: Vec<String>,
}

/// Fetch requests that will be deleted by delete_expired_requests (for pings).
pub async fn get_expired_requests(pool: &PgPool) -> Result<Vec<ExpiredRequest>, sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT mr.title,
        COALESCE(
            (SELECT array_agg(DISTINCT mrs.username) FROM media_request_subscribers mrs WHERE mrs.request_id = mr.id),
            ARRAY[]::TEXT[]
        ) as "subscribers!"
        FROM media_requests mr
        WHERE (status = 'pending' AND created_at < NOW() - INTERVAL '30 days')
           OR (status = 'fulfilled' AND updated_at < NOW() - INTERVAL '30 days')
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ExpiredRequest {
            title: r.title,
            subscribers: r.subscribers,
        })
        .collect())
}

fn guid_for_item_lookup(guid: &str) -> &str {
    guid.rsplit('/').next().unwrap_or(guid)
}

/// Stable key for the same catalog title (matches lookup normalization). Use when grouping rows that share a guid.
pub fn media_guid_key(guid: &str) -> String {
    guid_for_item_lookup(guid).to_string()
}

/// Check if a movie request is fulfilled in the database.
/// New movie: any copy counts; upgrade: must match requested resolution.
pub async fn is_movie_request_fulfilled(
    pool: &PgPool,
    guid: &str,
    requested_resolution: Option<&str>,
    is_upgrade: bool,
) -> Result<Option<String>, sqlx::Error> {
    let normalized_guid = guid_for_item_lookup(guid);

    if !is_upgrade {
        let exists = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM items WHERE guid = $1 AND item_type = 'movie') AS "exists!""#,
            normalized_guid
        )
        .fetch_one(pool)
        .await?;

        if exists {
            let res = sqlx::query_scalar!(
                r#"
                SELECT mp.video_resolution as "res?" FROM items i
                JOIN media_parts mp ON mp.item_id = i.id AND mp.server_id = i.server_id
                WHERE i.guid = $1 AND i.item_type = 'movie' AND mp.video_resolution IS NOT NULL
                GROUP BY mp.video_resolution ORDER BY COUNT(*) DESC LIMIT 1
                "#,
                normalized_guid
            )
            .fetch_optional(pool)
            .await?
            .flatten();
            return Ok(Some(res.unwrap_or_else(|| "Unknown".to_string())));
        }
        return Ok(None);
    }

    if let Some(req_res) = requested_resolution.filter(|r| !r.trim().is_empty()) {
        let pattern = match req_res.to_lowercase().as_str() {
            "4k" | "2160p" => "%4k%".to_string(),
            "1080p" | "1080" => "%1080%".to_string(),
            "720p" | "720" => "%720%".to_string(),
            r => format!("%{}%", r),
        };

        let res = sqlx::query_scalar!(
            r#"
            SELECT mp.video_resolution as "res?" FROM items i
            JOIN media_parts mp ON mp.item_id = i.id AND mp.server_id = i.server_id
            WHERE i.guid = $1 AND i.item_type = 'movie'
              AND (mp.video_resolution ILIKE $2 OR ($2 = '%4k%' AND mp.video_resolution ILIKE '%2160%'))
              AND mp.video_resolution IS NOT NULL
            GROUP BY mp.video_resolution ORDER BY COUNT(*) DESC LIMIT 1
            "#,
            normalized_guid,
            pattern
        )
        .fetch_optional(pool)
        .await?
        .flatten();
        Ok(res)
    } else {
        Ok(None)
    }
}

/// TV show: season presence only; resolution is the most common across episodes (for the ping).
pub async fn is_show_request_fulfilled(
    pool: &PgPool,
    guid: &str,
    requested_season: Option<i32>,
) -> Result<Option<String>, sqlx::Error> {
    let normalized_guid = guid_for_item_lookup(guid);

    let show_exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM items WHERE guid = $1 AND item_type = 'show') AS "exists!""#,
        normalized_guid
    )
    .fetch_one(pool)
    .await?;

    if !show_exists {
        return Ok(None);
    }

    if let Some(season_num) = requested_season {
        let season_exists = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM items show
                JOIN items season ON season.parent_id = show.id AND season.server_id = show.server_id
                    AND season.item_type = 'season' AND season.index = $2
                WHERE show.guid = $1 AND show.item_type = 'show'
            ) AS "exists!"
            "#,
            normalized_guid,
            season_num
        )
        .fetch_one(pool)
        .await?;

        if !season_exists {
            let direct_episodes = sqlx::query_scalar!(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM items show
                    JOIN items ep ON ep.parent_id = show.id AND ep.server_id = show.server_id AND ep.item_type = 'episode'
                    WHERE show.guid = $1 AND show.item_type = 'show'
                    AND NOT EXISTS (SELECT 1 FROM items s WHERE s.parent_id = show.id AND s.item_type = 'season')
                ) AS "exists!"
                "#,
                normalized_guid
            )
            .fetch_one(pool)
            .await?;

            if !(direct_episodes && season_num == 1) {
                return Ok(None);
            }
        }
    }

    if let Some(season_num) = requested_season {
        let res = sqlx::query_scalar!(
            r#"
            SELECT mp.video_resolution as "res?" FROM items show
            JOIN items season ON season.parent_id = show.id AND season.server_id = show.server_id AND season.item_type = 'season'
            JOIN items ep ON ep.parent_id = season.id AND ep.server_id = season.server_id AND ep.item_type = 'episode'
            JOIN media_parts mp ON mp.item_id = ep.id AND mp.server_id = ep.server_id
            WHERE show.guid = $1 AND show.item_type = 'show' AND season.index = $2 AND mp.video_resolution IS NOT NULL
            GROUP BY mp.video_resolution ORDER BY COUNT(*) DESC LIMIT 1
            "#,
            normalized_guid,
            season_num
        )
        .fetch_optional(pool)
        .await?
        .flatten();
        Ok(Some(res.unwrap_or_else(|| "Unknown".to_string())))
    } else {
        let res = sqlx::query_scalar!(
            r#"
            SELECT mp.video_resolution as "res?" FROM items show
            JOIN items ep ON ep.server_id = show.server_id AND ep.item_type = 'episode'
              AND (ep.parent_id = show.id OR EXISTS (SELECT 1 FROM items s WHERE s.id = ep.parent_id AND s.parent_id = show.id AND s.item_type = 'season'))
            JOIN media_parts mp ON mp.item_id = ep.id AND mp.server_id = ep.server_id
            WHERE show.guid = $1 AND show.item_type = 'show' AND mp.video_resolution IS NOT NULL
            GROUP BY mp.video_resolution ORDER BY COUNT(*) DESC LIMIT 1
            "#,
            normalized_guid
        )
        .fetch_optional(pool)
        .await?
        .flatten();
        Ok(Some(res.unwrap_or_else(|| "Unknown".to_string())))
    }
}
