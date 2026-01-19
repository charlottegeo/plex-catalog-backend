use crate::models::{
    DbServer, Device, EpisodeDetails, Item, Library, Media, MediaDetails, MediaVersion, Part,
    SearchResult, SeasonSummary, ServerAvailability, Stream,
};
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};

#[derive(FromRow)]
struct ItemByGuid {
    id: String,
    server_id: String,
    server_name: String,
    guid: Option<String>,
    title: String,
    summary: Option<String>,
    year: Option<i16>,
    art_path: Option<String>,
    thumb_path: Option<String>,
    item_type: String,
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
) -> Result<Vec<Item>, sqlx::Error> {
    sqlx::query(
        r#"
        SELECT id, guid, parent_id, title, summary, item_type, year, thumb_path, art_path, index, leaf_count
        FROM items
        WHERE server_id = $1 AND library_id = $2 AND item_type IN ('movie', 'show')
        ORDER BY title ASC
        "#,
    )
    .bind(server_id)
    .bind(library_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|row| {
        use sqlx::Row;
        let id: String = row.try_get("id")?;
        Ok(Item {
            guid: row.try_get("guid")?,
            rating_key: id.clone(),
            key: format!("/library/metadata/{}", id),
            parent_id: row.try_get("parent_id")?,
            title: row.try_get("title")?,
            summary: row.try_get("summary")?,
            item_type: row.try_get("item_type")?,
            year: row.try_get::<i16, _>("year").unwrap_or(0) as u16,
            media: Vec::new(),
            thumb: row.try_get("thumb_path")?,
            art: row.try_get("art_path")?,
            index: row.try_get("index")?,
            leaf_count: row.try_get("leaf_count")?,
        })
    })
    .collect()
}

pub async fn upsert_server(
    pool: &PgPool,
    server: &Device,
    is_online: bool,
    sync_time: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let access_token = server.access_token.as_deref().unwrap_or_default();
    let connection_uri = server
        .connections
        .iter()
        .find(|c| !c.local)
        .map_or("", |c| &c.uri);

    sqlx::query!(
        r#"
        INSERT INTO servers (id, name, access_token, connection_uri, last_seen, is_online)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (id) DO UPDATE SET
            last_seen = EXCLUDED.last_seen,
            is_online = EXCLUDED.is_online,
            name = CASE WHEN servers.name IS DISTINCT FROM EXCLUDED.name THEN EXCLUDED.name ELSE servers.name END,
            access_token = CASE WHEN servers.access_token IS DISTINCT FROM EXCLUDED.access_token THEN EXCLUDED.access_token ELSE servers.access_token END,
            connection_uri = CASE WHEN servers.connection_uri IS DISTINCT FROM EXCLUDED.connection_uri THEN EXCLUDED.connection_uri ELSE servers.connection_uri END
        "#,
        server.client_identifier,
        server.name,
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
        .map(|i| {
            i.item
                .summary
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|s| s.clone())
        })
        .collect();
    let item_types: Vec<String> = items.iter().map(|i| i.item.item_type.clone()).collect();
    let years: Vec<i16> = items.iter().map(|i| i.item.year as i16).collect();
    let thumb_paths: Vec<Option<String>> = items
        .iter()
        .map(|i| i.item.thumb.as_ref().map(|s| s.clone()))
        .collect();
    let art_paths: Vec<Option<String>> = items
        .iter()
        .map(|i| i.item.art.as_ref().map(|s| s.clone()))
        .collect();
    let guids: Vec<Option<String>> = items
        .iter()
        .map(|i| i.item.guid.as_ref().map(|s| s.clone()))
        .collect();
    let indices: Vec<Option<i32>> = items.iter().map(|i| i.item.index).collect();
    let leaf_counts: Vec<Option<i32>> = items.iter().map(|i| i.item.leaf_count).collect();
    let sync_times: Vec<DateTime<Utc>> = vec![sync_time; items.len()];

    let result = sqlx::query(
        r#"
        INSERT INTO items (id, library_id, server_id, parent_id, title, summary, item_type, year, thumb_path, art_path, last_seen, guid, index, leaf_count)
        SELECT * FROM UNNEST(
            $1::text[],
            $2::text[],
            $3::text[],
            $4::text[],
            $5::text[],
            $6::text[],
            $7::text[],
            $8::smallint[],
            $9::text[],
            $10::text[],
            $11::timestamptz[],
            $12::text[],
            $13::integer[],
            $14::integer[]
        ) AS t(id, library_id, server_id, parent_id, title, summary, item_type, year, thumb_path, art_path, last_seen, guid, index, leaf_count)
        ON CONFLICT (id, server_id) DO UPDATE SET
            last_seen = EXCLUDED.last_seen,
            title = CASE WHEN items.title IS DISTINCT FROM EXCLUDED.title THEN EXCLUDED.title ELSE items.title END,
            summary = CASE WHEN items.summary IS DISTINCT FROM EXCLUDED.summary THEN EXCLUDED.summary ELSE items.summary END,
            year = CASE WHEN items.year IS DISTINCT FROM EXCLUDED.year THEN EXCLUDED.year ELSE items.year END,
            thumb_path = CASE WHEN items.thumb_path IS DISTINCT FROM EXCLUDED.thumb_path THEN EXCLUDED.thumb_path ELSE items.thumb_path END,
            art_path = CASE WHEN items.art_path IS DISTINCT FROM EXCLUDED.art_path THEN EXCLUDED.art_path ELSE items.art_path END,
            guid = CASE WHEN items.guid IS DISTINCT FROM EXCLUDED.guid THEN EXCLUDED.guid ELSE items.guid END,
            index = CASE WHEN items.index IS DISTINCT FROM EXCLUDED.index THEN EXCLUDED.index ELSE items.index END,
            leaf_count = CASE WHEN items.leaf_count IS DISTINCT FROM EXCLUDED.leaf_count THEN EXCLUDED.leaf_count ELSE items.leaf_count END
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
    .execute(pool)
    .await?;

    Ok(result.rows_affected() as usize)
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
    println!("Pruning old data from database...");
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
    println!(
        "Pruning complete. Removed: {} servers, {} libraries, {} items, {} parts, {} streams.",
        servers_deleted, libraries_deleted, items_deleted, parts_deleted, streams_deleted
    );
    Ok(())
}

pub async fn search_items(pool: &PgPool, query: &str) -> Result<Vec<SearchResult>, sqlx::Error> {
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
            i.guid,
            i.title, i.summary, i.item_type, i.year, i.thumb_path,
            s.id as "server_id!",
            s.name as "server_name!",
            ts_rank(i.fts_document, to_tsquery('simple', $1)) as rank
        FROM items i
        JOIN servers s ON i.server_id = s.id
        WHERE
            i.guid IS NOT NULL AND
            i.fts_document @@ to_tsquery('simple', $1)
            AND i.item_type IN ('movie', 'show')
        ORDER BY rank DESC
        LIMIT 50
        "#,
        fts_query
    )
    .fetch_all(pool)
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
    let items = sqlx::query_as!(
        ItemByGuid,
        r#"
        SELECT i.id, i.server_id, s.name as "server_name!", i.guid as "guid?", i.title, i.summary, i.year, i.art_path, i.thumb_path, i.item_type
        FROM items i
        JOIN servers s ON i.server_id = s.id
        WHERE i.guid = $1
        "#,
        guid
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
            versions: server_versions,
        });
    }

    Ok(Some(MediaDetails {
        guid: first_item.guid.clone().unwrap_or_default(),
        title: first_item.title.clone(),
        summary: first_item.summary.clone(),
        year: first_item.year,
        art_path: first_item.art_path.clone(),
        thumb_path: first_item.thumb_path.clone(),
        item_type: first_item.item_type.clone(),
        available_on,
    }))
}

pub async fn get_show_seasons(
    pool: &PgPool,
    show_id: &str,
    server_id: &str,
) -> Result<Vec<SeasonSummary>, sqlx::Error> {
    let mut seasons = sqlx::query_as!(
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

    let has_direct_episodes = sqlx::query!(
        "SELECT COUNT(*) as count FROM items WHERE parent_id = $1 AND server_id = $2 AND item_type = 'episode'",
        show_id,
        server_id
    )
    .fetch_one(pool)
    .await?
    .count.unwrap_or(0) > 0;

    if seasons.is_empty() || has_direct_episodes {
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
            if !seasons.iter().any(|s| s.id == show.id) {
                seasons.push(show);
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
                e.parent_id = (SELECT parent_id FROM items WHERE id = $1 AND item_type = 'season')
                AND EXISTS (SELECT 1 FROM items WHERE id = $1 AND item_type = 'season')
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
