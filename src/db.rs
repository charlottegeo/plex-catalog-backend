use crate::models::{
    DbServer, Device, EpisodeDetails, Item, Library, Media, MediaDetails, MediaVersion, Part,
    SearchResult, SeasonSummary, ServerAvailability, Stream,
};
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};

type Transaction<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

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
    item_id: String,
    video_resolution: Option<String>,
    subtitle_language: Option<String>,
}

#[derive(FromRow)]
struct EpisodeWithVersion {
    id: String,
    title: String,
    summary: Option<String>,
    thumb_path: Option<String>,
    video_resolution: Option<String>,
    subtitle_language: Option<String>,
}

pub async fn get_all_servers(pool: &PgPool) -> Result<Vec<DbServer>, sqlx::Error> {
    sqlx::query_as!(DbServer, "SELECT * FROM servers ORDER BY name")
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
            thumb: row.try_get("thumb_path")?,
            art: row.try_get("art_path")?,
            index: row.try_get("index")?,
            leaf_count: row.try_get("leaf_count")?,
        })
    })
    .collect()
}
pub async fn upsert_server(
    tx: &mut Transaction<'_>,
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
            name = EXCLUDED.name,
            access_token = EXCLUDED.access_token,
            connection_uri = EXCLUDED.connection_uri,
            last_seen = EXCLUDED.last_seen,
            is_online = EXCLUDED.is_online
        "#,
        server.client_identifier,
        server.name,
        access_token,
        connection_uri,
        sync_time,
        is_online
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn upsert_library(
    tx: &mut Transaction<'_>,
    library: &Library,
    server_id: &str,
    sync_time: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO libraries (id, server_id, title, library_type, last_seen)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (id, server_id) DO UPDATE SET
            title = EXCLUDED.title,
            library_type = EXCLUDED.library_type,
            last_seen = EXCLUDED.last_seen
        "#,
        library.key,
        server_id,
        library.title,
        library.library_type,
        sync_time
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn upsert_item(
    tx: &mut Transaction<'_>,
    item: &Item,
    library_id: &str,
    server_id: &str,
    sync_time: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO items (id, library_id, server_id, parent_id, title, summary, item_type, year, thumb_path, art_path, last_seen, guid, index, leaf_count)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
        ON CONFLICT (id, server_id) DO UPDATE SET
            title = EXCLUDED.title,
            summary = EXCLUDED.summary,
            year = EXCLUDED.year,
            thumb_path = EXCLUDED.thumb_path,
            art_path = EXCLUDED.art_path,
            last_seen = EXCLUDED.last_seen,
            guid = EXCLUDED.guid,
            index = EXCLUDED.index,
            leaf_count = EXCLUDED.leaf_count
        "#,
        item.rating_key,
        library_id,
        server_id,
        item.parent_id,
        item.title,
        item.summary,
        item.item_type,
        item.year as i16,
        item.thumb,
        item.art,
        sync_time,
        item.guid,
        item.index,
        item.leaf_count
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn upsert_media_part(
    tx: &mut Transaction<'_>,
    part: &Part,
    item_id: &str,
    server_id: &str,
    media: &Media,
    sync_time: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO media_parts (id, item_id, server_id, video_resolution, last_seen)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (id, server_id) DO UPDATE SET
            video_resolution = EXCLUDED.video_resolution,
            last_seen = EXCLUDED.last_seen
        "#,
        part.id,
        item_id,
        server_id,
        media.video_resolution,
        sync_time
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

pub async fn upsert_stream(
    tx: &mut Transaction<'_>,
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
            language = EXCLUDED.language,
            language_code = EXCLUDED.language_code,
            format = EXCLUDED.format,
            last_seen = EXCLUDED.last_seen
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
    .execute(&mut **tx).await?;
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
        let unique_resolutions: std::collections::HashSet<_> = versions
            .iter()
            .filter(|v| v.item_id == item.id && v.video_resolution.is_some())
            .map(|v| v.video_resolution.as_ref().unwrap())
            .collect();
        for resolution in unique_resolutions {
            let subtitles: Vec<String> = versions
                .iter()
                .filter(|v| {
                    v.item_id == item.id && v.video_resolution.as_deref() == Some(resolution)
                })
                .filter_map(|v| v.subtitle_language.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            server_versions.push(MediaVersion {
                video_resolution: resolution.to_string(),
                subtitles,
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
            id, title, summary, thumb_path, art_path, leaf_count
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
        let show_as_season = sqlx::query_as!(
            SeasonSummary,
            r#"
            SELECT
                id, title, summary, thumb_path, art_path, leaf_count
            FROM items
            WHERE id = $1 AND server_id = $2 AND item_type = 'show' AND leaf_count > 0
            "#,
            show_id,
            server_id
        )
        .fetch_optional(pool)
        .await?;

        if let Some(show) = show_as_season {
            seasons.push(show);
        }
    }

    Ok(seasons)
}

pub async fn get_season_episodes(
    pool: &PgPool,
    parent_id: &str,
    server_id: &str,
) -> Result<Vec<EpisodeDetails>, sqlx::Error> {
    let rows = sqlx::query_as!(
        EpisodeWithVersion,
        r#"
        SELECT
            e.id, e.title, e.summary, e.thumb_path,
            mp.video_resolution, s.language as "subtitle_language"
        FROM items e
        LEFT JOIN media_parts mp ON mp.item_id = e.id AND mp.server_id = e.server_id
        LEFT JOIN streams s ON s.media_part_id = mp.id AND s.server_id = mp.server_id AND s.stream_type = 3
        WHERE e.parent_id = $1 AND e.server_id = $2 AND e.item_type = 'episode'
        ORDER BY e.index ASC
        "#,
        parent_id,
        server_id
    )
    .fetch_all(pool)
    .await?;
    let mut episodes: Vec<EpisodeDetails> = Vec::new();

    for row in rows {
        if let Some(last_episode) = episodes.last_mut() {
            if last_episode.id == row.id {
                if let Some(resolution) = row.video_resolution {
                    if let Some(version) = last_episode
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
                        last_episode.versions.push(MediaVersion {
                            video_resolution: resolution,
                            subtitles: row.subtitle_language.map_or(vec![], |lang| vec![lang]),
                        });
                    }
                }
                continue;
            }
        }
        let mut new_episode = EpisodeDetails {
            id: row.id.clone(),
            title: row.title.clone(),
            summary: row.summary.clone(),
            thumb_path: row.thumb_path.clone(),
            versions: Vec::new(),
        };

        if let Some(resolution) = row.video_resolution {
            new_episode.versions.push(MediaVersion {
                video_resolution: resolution,
                subtitles: row.subtitle_language.map_or(vec![], |lang| vec![lang]),
            });
        }

        episodes.push(new_episode);
    }

    Ok(episodes)
}
