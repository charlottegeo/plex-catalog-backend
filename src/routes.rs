use crate::auth::{CSHAuth, User};
#[allow(unused_imports)]
use crate::{
    db,
    error::ApiError,
    models::{
        CatalogSearchQuery, DbServer, DiscoverResultsPage, GenreCount, GenresQuery,
        GlobalImageQuery, ImageQuery, LibraryItemsPage, LibraryItemsQuery, MediaRequest,
        MediaRequestPayload, RequestsQuery, SearchQuery, SearchResultsPage,
    },
    AppState,
};
use actix_web::{
    delete, get, http::header, post, web, HttpMessage, HttpRequest, HttpResponse, Responder, Result,
};
use std::collections::HashSet;

async fn get_server_details_or_404(
    db_pool: &sqlx::PgPool,
    server_id: &str,
) -> Result<DbServer, ApiError> {
    db::get_server_details(db_pool, server_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Server not found in database".to_string()))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/api")
            .wrap(CSHAuth::enabled())
            .service(get_servers_handler)
            .service(get_system_info_handler)
            .service(get_libraries_handler)
            .service(get_library_items_handler)
            .service(get_item_details_handler)
            .service(get_item_children_handler)
            .service(get_item_extras_handler)
            .service(get_image_handler)
            .service(get_global_image_handler)
            .service(search_handler)
            .service(get_genres_handler)
            .service(discover_handler)
            .service(discover_item_handler)
            .service(create_request_handler)
            .service(subscribe_request_handler)
            .service(get_all_requests_handler)
            .service(get_active_requests_by_guid_handler)
            .service(delete_request_handler)
            .service(unsubscribe_request_handler)
            .service(get_media_details_handler)
            .service(get_seasons_handler)
            .service(get_episodes_handler),
    );
}

/// List all Plex servers known to the catalog.
///
/// Each server includes "status" / "statusSince" (see "server_status" module).
#[utoipa::path(
    get,
    path = "/api/servers",
    responses((status = 200, description = "List of servers", body = Vec<DbServer>))
)]
#[get("/servers")]
async fn get_servers_handler(state: web::Data<AppState>) -> Result<impl Responder, ApiError> {
    let servers = db::get_all_servers(&state.db_pool).await?;
    Ok(HttpResponse::Ok().json(servers))
}

/// Get system and sync health information.
///
/// Returns counts of movies and shows, server counts, last sync time, sync interval, last error (if any), and whether a sync is in progress.
#[utoipa::path(
    get,
    path = "/api/system/info",
    responses((status = 200, description = "System info", body = SystemInfo))
)]
#[get("/system/info")]
async fn get_system_info_handler(state: web::Data<AppState>) -> Result<impl Responder, ApiError> {
    let info = db::get_system_info(&state.db_pool, state.sync_interval_minutes).await?;
    Ok(HttpResponse::Ok().json(info))
}

/// List libraries (sections) for a server.
///
/// Returns movie/show libraries for the given server ID from the catalog.
#[utoipa::path(
    get,
    path = "/api/servers/{server_id}/libraries",
    params(("server_id" = String, Path, description = "Server ID (Plex client identifier)")),
    responses(
        (status = 200, description = "List of libraries", body = Vec<Library>),
        (status = 404, description = "Server not found in catalog")
    )
)]
#[get("/servers/{server_id}/libraries")]
async fn get_libraries_handler(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let server_id = path.into_inner();
    let libraries = db::get_server_libraries(&state.db_pool, &server_id).await?;
    Ok(HttpResponse::Ok().json(libraries))
}

/// List top-level items in a library.
///
/// Returns movies and shows in the given library section, optionally filtered by genre and type and sorted. Use children or details endpoints to get more information.
#[utoipa::path(
    get,
    path = "/api/servers/{server_id}/libraries/{library_key}",
    params(
        ("server_id" = String, Path, description = "Server ID (Plex client identifier)"),
        ("library_key" = String, Path, description = "Library section key from /libraries"),
        ("genre" = Option<String>, Query, description = "Only return titles with this genre (exact name from /api/genres)"),
        ("type" = Option<String>, Query, description = "movie | show (default both)"),
        ("sort" = Option<String>, Query, description = "title | year | releaseDate | duration (default title)"),
        ("order" = Option<String>, Query, description = "asc | desc (default asc)"),
        ("limit" = Option<u32>, Query, description = "Max results to return"),
        ("offset" = Option<u32>, Query, description = "Result offset (for pagination/infinite scroll)")
    ),
    responses(
        (status = 200, description = "Paginated library items", body = LibraryItemsPage),
        (status = 400, description = "Invalid parameter"),
        (status = 404, description = "Server not found")
    )
)]
#[get("/servers/{server_id}/libraries/{library_key}")]
async fn get_library_items_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
    query: web::Query<LibraryItemsQuery>,
) -> Result<impl Responder, ApiError> {
    let (server_id, library_key) = path.into_inner();
    let limit = query.limit.unwrap_or(50) as i64;
    let offset = query.offset.unwrap_or(0) as i64;
    let filters = db::CatalogFilters {
        fts_query: None,
        genre: parse_genre(query.genre.as_deref()),
        item_type: parse_item_type(query.item_type.as_deref())?,
        sort: parse_sort(query.sort.as_deref(), false)?,
        descending: parse_order(query.order.as_deref())?,
        server_id: Some(server_id),
        library_id: Some(library_key),
    };
    let total = db::get_library_items_total(&state.db_pool, &filters).await?;
    let items = db::get_library_items(&state.db_pool, &filters, limit, offset).await?;
    Ok(HttpResponse::Ok().json(LibraryItemsPage {
        total,
        limit,
        offset,
        items,
    }))
}

/// Get full metadata for a single item.
///
/// Fetches live from the catalog. Includes media parts and streams. Use rating_key from list/children responses.
#[utoipa::path(
    get,
    path = "/api/servers/{server_id}/items/{rating_key}",
    params(
        ("server_id" = String, Path, description = "Server ID (Plex client identifier)"),
        ("rating_key" = String, Path, description = "Plex rating key of the item (e.g. from list/children)")
    ),
    responses(
        (status = 200, description = "Item details", body = ItemWithDetails),
        (status = 404, description = "Server or item not found")
    )
)]
#[get("/servers/{server_id}/items/{rating_key}")]
async fn get_item_details_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, rating_key) = path.into_inner();
    let client = &state.plex_client;
    let server_details = get_server_details_or_404(&state.db_pool, &server_id).await?;

    let item_option = client
        .get_item_details(
            &server_details.connection_uri,
            &server_details.access_token,
            &rating_key,
        )
        .await?;

    match item_option {
        Some(item) => Ok(HttpResponse::Ok().json(item)),
        None => Err(ApiError::NotFound("Item details not found".to_string())),
    }
}

/// List bonus features (extras) for a media item.
///
/// Returns extras from the catalog (e.g. trailers, behind-the-scenes). Parent can be a movie, show, season, or episode.
#[utoipa::path(
    get,
    path = "/api/servers/{server_id}/items/{rating_key}/extras",
    params(
        ("server_id" = String, Path, description = "Server ID (Plex client identifier)"),
        ("rating_key" = String, Path, description = "Plex rating key of the parent movie/show/season/episode")
    ),
    responses(
        (status = 200, description = "List of extras", body = Vec<PlexExtra>),
        (status = 404, description = "Server not found")
    )
)]
#[get("/servers/{server_id}/items/{rating_key}/extras")]
async fn get_item_extras_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, rating_key) = path.into_inner();
    let server = get_server_details_or_404(&state.db_pool, &server_id).await?;
    let extras = state
        .plex_client
        .get_item_extras(&server.connection_uri, &server.access_token, &rating_key)
        .await?;
    Ok(HttpResponse::Ok().json(extras))
}

/// List child items of a media item.
///
/// Fetches from the catalog. For a show returns seasons; for a season returns episodes. Used to build the browse tree.
#[utoipa::path(
    get,
    path = "/api/servers/{server_id}/items/{rating_key}/children",
    params(
        ("server_id" = String, Path, description = "Server ID (Plex client identifier)"),
        ("rating_key" = String, Path, description = "Plex rating key of the parent (show or season)")
    ),
    responses(
        (status = 200, description = "List of child items", body = ItemList),
        (status = 404, description = "Server not found")
    )
)]
#[get("/servers/{server_id}/items/{rating_key}/children")]
async fn get_item_children_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, rating_key) = path.into_inner();
    let client = &state.plex_client;
    let server_details = get_server_details_or_404(&state.db_pool, &server_id).await?;

    let children = client
        .get_item_children(
            &server_details.connection_uri,
            &server_details.access_token,
            &rating_key,
        )
        .await?;

    Ok(HttpResponse::Ok().json(children))
}

/// Proxy or resize an image from a Plex server.
///
/// Images are cached. Optional "width" and "height" query params trigger transcoding to that size.
#[utoipa::path(
    get,
    path = "/api/servers/{server_id}/image/{image_path}",
    params(
        ("server_id" = String, Path, description = "Server ID (Plex client identifier)"),
        ("image_path" = String, Path, description = "Image path (may contain slashes, e.g. library/metadata/123/thumb/456)"),
        ("width" = Option<u32>, Query, description = "Optional width in pixels for transcoded image"),
        ("height" = Option<u32>, Query, description = "Optional height in pixels for transcoded image")
    ),
    responses(
        (status = 200, description = "Image binary (JPEG)"),
        (status = 404, description = "Server not found")
    )
)]
#[get("/servers/{server_id}/image/{image_path:.*}")]
async fn get_image_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
    query: web::Query<ImageQuery>,
) -> Result<impl Responder, ApiError> {
    let (server_id, image_path) = path.into_inner();
    let width = query.width;
    let height = query.height;

    let cache_key = if let (Some(w), Some(h)) = (width, height) {
        format!("{}/{}_{}x{}", server_id, image_path, w, h)
    } else {
        format!("{}/{}", server_id, image_path)
    };

    if let Some(cached_image) = state.image_cache.get(&cache_key).await {
        return Ok(HttpResponse::Ok()
            .content_type(cached_image.content_type.as_str())
            .insert_header(header::CacheControl(vec![header::CacheDirective::MaxAge(
                3600u32,
            )]))
            .body(cached_image.bytes));
    }

    let server_details = get_server_details_or_404(&state.db_pool, &server_id).await?;
    let client = &state.plex_client;
    let mut image_response = client
        .get_image(
            &server_details.connection_uri,
            &server_details.access_token,
            &image_path,
            width,
            height,
        )
        .await;

    if image_response.is_err() && (width.is_some() || height.is_some()) {
        image_response = client
            .get_image(
                &server_details.connection_uri,
                &server_details.access_token,
                &image_path,
                None,
                None,
            )
            .await;
    }
    let image_response = image_response.map_err(ApiError::from)?;

    let content_type = image_response
        .headers()
        .get("content-type")
        .and_then(|val| val.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let image_bytes = image_response.bytes().await.map_err(ApiError::from)?;

    let cached_image = crate::models::CachedImage {
        bytes: image_bytes.clone(),
        content_type: content_type.clone(),
    };

    state.image_cache.insert(cache_key, cached_image).await;

    Ok(HttpResponse::Ok()
        .content_type(content_type)
        .insert_header(header::CacheControl(vec![header::CacheDirective::MaxAge(
            3600u32,
        )]))
        .body(image_bytes))
}

/// Get an image from metadata.provider.plex.tv
#[utoipa::path(
    get,
    path = "/api/image/global",
    params(("url" = String, Query, description = "URL of the image to fetch")),
    responses((status = 200, description = "Image binary (JPEG)"))
)]
#[get("/image/global")]
async fn get_global_image_handler(
    state: web::Data<AppState>,
    query: web::Query<GlobalImageQuery>,
) -> Result<impl Responder, ApiError> {
    let parsed_url = match reqwest::Url::parse(&query.url) {
        Ok(url) => url,
        Err(_) => {
            return Ok(HttpResponse::BadRequest().body("Invalid image URL"));
        }
    };

    if parsed_url.scheme() != "https" && parsed_url.scheme() != "http" {
        return Ok(HttpResponse::BadRequest().body("Invalid image URL scheme"));
    }

    let Some(host) = parsed_url.host_str() else {
        return Ok(HttpResponse::BadRequest().body("Image URL is missing a host"));
    };

    if host != "metadata.provider.plex.tv" {
        return Ok(
            HttpResponse::BadRequest().body("Only metadata.provider.plex.tv URLs are allowed")
        );
    }

    let response = match state
        .plex_client
        .get_global_image(parsed_url.as_ref())
        .await
    {
        Ok(response) => response,
        Err(_) => return Ok(HttpResponse::NotFound().body("Unable to fetch global image")),
    };

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|val| val.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();

    let image_bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(_) => return Ok(HttpResponse::NotFound().body("Unable to fetch global image")),
    };

    Ok(HttpResponse::Ok()
        .content_type(content_type)
        .insert_header(header::CacheControl(vec![header::CacheDirective::MaxAge(
            86400u32, //Caches for a day, might change later
        )]))
        .body(image_bytes))
}

/// Search Plex's global discover catalog for movies and TV shows.
///
/// Uses the Plex account token to query metadata.provider.plex.tv.
#[utoipa::path(
    get,
    path = "/api/discover",
    params(
        ("q" = String, Query, description = "Search query"),
        ("limit" = Option<u32>, Query, description = "Max results to return"),
        ("offset" = Option<u32>, Query, description = "Result offset (for pagination/infinite scroll)")
    ),
    responses((status = 200, description = "Paginated discover results", body = DiscoverResultsPage))
)]
#[get("/discover")]
async fn discover_handler(
    state: web::Data<AppState>,
    query: web::Query<SearchQuery>,
) -> Result<impl Responder, ApiError> {
    let limit = query.limit.unwrap_or(30) as i64;
    let offset = query.offset.unwrap_or(0) as i64;
    let results = state
        .plex_client
        .search_global_discover(&query.q, limit as u32, offset as u32)
        .await?;

    let items = results
        .get("MediaContainer")
        .and_then(|mc| mc.get("Metadata"))
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();

    let discovered_total = results
        .get("MediaContainer")
        .and_then(|mc| {
            mc.get("totalSize")
                .or_else(|| mc.get("total"))
                .or_else(|| mc.get("size"))
        })
        .and_then(|v| v.as_i64());

    let total = discovered_total.unwrap_or_else(|| offset + items.len() as i64);

    Ok(HttpResponse::Ok().json(DiscoverResultsPage {
        total,
        limit,
        offset,
        items,
    }))
}

#[utoipa::path(
    get,
    path = "/api/discover/{guid}",
    params(("guid" = String, Path, description = "Plex GUID of the item")),
    responses((status = 200, description = "Item details from Discover", body = serde_json::Value))
)]
#[get("/discover/{guid:.*}")]
async fn discover_item_handler(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let guid = path.into_inner();
    let result = state.plex_client.get_discover_item_by_guid(&guid).await?;
    Ok(HttpResponse::Ok().json(result))
}

/// Create or update a media request.
///
/// On conflict with an existing pending request for the same (username, guid), merges seasons and updates resolution.
#[utoipa::path(
    post,
    path = "/api/requests",
    request_body = MediaRequestPayload,
    responses((status = 200, description = "Created or updated requests", body = Vec<MediaRequest>))
)]
#[post("/requests")]
async fn create_request_handler(
    state: web::Data<AppState>,
    req: HttpRequest,
    payload: web::Json<MediaRequestPayload>,
) -> Result<impl Responder, ApiError> {
    let username = req
        .extensions()
        .get::<User>()
        .map(|u| u.preferred_username.clone())
        .unwrap();
    let mut payload = payload.into_inner();
    payload.guid = payload
        .guid
        .rsplit('/')
        .next()
        .unwrap_or(&payload.guid)
        .to_string();

    let mut created_requests = Vec::new();
    let is_upgrade = db::item_exists_by_guid(&state.db_pool, &payload.guid).await?;

    let seasons = if payload.item_type == "show" {
        payload.requested_seasons.clone().unwrap_or_default()
    } else {
        vec![]
    };

    let target_seasons = if seasons.is_empty() {
        vec![None]
    } else {
        seasons.into_iter().map(Some).collect()
    };

    let mut new_seasons_for_ping = Vec::new();
    let mut needs_ping = false;

    for season in target_seasons {
        let (request, is_new) = db::create_or_subscribe_request(
            &state.db_pool,
            &username,
            &payload,
            season,
            is_upgrade,
        )
        .await?;
        created_requests.push(request);

        if is_new {
            needs_ping = true;
            if let Some(s) = season {
                new_seasons_for_ping.push(s);
            }
        }
    }

    if needs_ping {
        if let Ok(servers) = db::get_all_servers(&state.db_pool).await {
            let mut unique_owners = HashSet::new();
            for server in servers {
                if let Some(owner) = server.owner_username.filter(|o| !o.trim().is_empty()) {
                    unique_owners.insert(owner);
                }
            }

            let seasons_slice = if new_seasons_for_ping.is_empty() {
                None
            } else {
                Some(new_seasons_for_ping.as_slice())
            };

            for owner in unique_owners {
                if let Ok(Some(csh_uid)) = state.oidc_client.get_csh_uid_by_plex(&owner).await {
                    let _ = state
                        .ping_client
                        .send_request_ping(
                            &csh_uid,
                            &username,
                            &payload.title,
                            payload.year,
                            &payload.item_type,
                            seasons_slice,
                            payload.requested_resolution.as_deref(),
                            is_upgrade,
                        )
                        .await;
                }
            }
        }
    }

    Ok(HttpResponse::Ok().json(created_requests))
}

/// List media requests (pending and/or fulfilled), optionally filtered and sorted by date.
#[utoipa::path(
    get,
    path = "/api/requests",
    params(
        ("status" = Option<String>, Query, description = "Filter: pending | fulfilled"),
        ("sortBy" = Option<String>, Query, description = "createdAt (default) | fulfilledAt"),
        ("sortOrder" = Option<String>, Query, description = "asc | desc (default desc)")
    ),
    responses((status = 200, description = "List of requests", body = Vec<MediaRequest>))
)]
#[get("/requests")]
async fn get_all_requests_handler(
    state: web::Data<AppState>,
    query: web::Query<RequestsQuery>,
) -> Result<impl Responder, ApiError> {
    let status_filter = query.status.as_deref().and_then(|s| match s {
        "pending" | "fulfilled" => Some(s),
        _ => None,
    });
    let sort_by = match query.sort_by.as_deref() {
        Some("fulfilledAt") => "fulfilledAt",
        _ => "createdAt",
    };
    let sort_desc = !matches!(query.sort_order.as_deref(), Some("asc"));
    let requests = db::get_all_requests(&state.db_pool, status_filter, sort_by, sort_desc).await?;
    Ok(HttpResponse::Ok().json(requests))
}

/// List active requests for a specific media guid.
#[utoipa::path(
    get,
    path = "/api/requests/media/{guid}",
    params(("guid" = String, Path, description = "Media GUID (full or normalized)")),
    responses((status = 200, description = "List of active requests for this media", body = Vec<MediaRequest>))
)]
#[get("/requests/media/{guid:.*}")]
async fn get_active_requests_by_guid_handler(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let guid = path.into_inner();
    let requests = db::get_active_requests_by_guid(&state.db_pool, &guid).await?;
    Ok(HttpResponse::Ok().json(requests))
}

/// Remove the current user from a request (same as unsubscribe). Deletes the request if no subscribers remain.
#[utoipa::path(
    delete,
    path = "/api/requests/{id}",
    params(("id" = i32, Path, description = "Request ID")),
    responses((status = 200, description = "Removed"), (status = 404, description = "Not subscribed"))
)]
#[delete("/requests/{id}")]
async fn delete_request_handler(
    state: web::Data<AppState>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<impl Responder, ApiError> {
    let username = req
        .extensions()
        .get::<User>()
        .map(|u| u.preferred_username.clone())
        .unwrap();
    let id = path.into_inner();
    let rows = db::unsubscribe_request(&state.db_pool, id, &username).await?;
    if rows == 0 {
        return Err(ApiError::NotFound(
            "Not subscribed to this request".to_string(),
        ));
    }
    Ok(HttpResponse::Ok().finish())
}

#[utoipa::path(
    post,
    path = "/api/requests/{id}/subscribe",
    params(("id" = i32, Path, description = "Request ID to subscribe to")),
    responses((status = 200, description = "Subscribed"))
)]
#[post("/requests/{id}/subscribe")]
async fn subscribe_request_handler(
    state: web::Data<AppState>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<impl Responder, ApiError> {
    let username = req
        .extensions()
        .get::<User>()
        .map(|u| u.preferred_username.clone())
        .unwrap();
    let request_id = path.into_inner();
    sqlx::query!("INSERT INTO media_request_subscribers (request_id, username) VALUES ($1, $2) ON CONFLICT DO NOTHING", request_id, username).execute(&state.db_pool).await?;
    Ok(HttpResponse::Ok().finish())
}

#[utoipa::path(
    delete,
    path = "/api/requests/{id}/subscribe",
    params(("id" = i32, Path, description = "Request ID to unsubscribe from")),
    responses((status = 200, description = "Unsubscribed"), (status = 404, description = "Not subscribed"))
)]
#[delete("/requests/{id}/subscribe")]
async fn unsubscribe_request_handler(
    state: web::Data<AppState>,
    req: HttpRequest,
    path: web::Path<i32>,
) -> Result<impl Responder, ApiError> {
    let username = req
        .extensions()
        .get::<User>()
        .map(|u| u.preferred_username.clone())
        .unwrap();
    let id = path.into_inner();
    let rows_affected = db::unsubscribe_request(&state.db_pool, id, &username).await?;

    if rows_affected == 0 {
        return Err(ApiError::NotFound(
            "Not subscribed to this request".to_string(),
        ));
    }
    Ok(HttpResponse::Ok().finish())
}

/// List all genres in the catalog.
///
/// Returns every genre used by at least one movie or show, with the number of distinct titles that have it.
#[utoipa::path(
    get,
    path = "/api/genres",
    params(("type" = Option<String>, Query, description = "Only count titles of this type: movie | show")),
    responses(
        (status = 200, description = "List of genres", body = Vec<GenreCount>),
        (status = 400, description = "Invalid type")
    )
)]
#[get("/genres")]
async fn get_genres_handler(
    state: web::Data<AppState>,
    query: web::Query<GenresQuery>,
) -> Result<impl Responder, ApiError> {
    let item_type = parse_item_type(query.item_type.as_deref())?;
    let cache_key = item_type.clone().unwrap_or_else(|| "all".to_string());
    let genres = match state.genre_cache.get(&cache_key).await {
        Some(cached) => cached,
        None => {
            let fresh = db::get_all_genres(&state.db_pool, item_type.as_deref()).await?;
            state.genre_cache.insert(cache_key, fresh.clone()).await;
            fresh
        }
    };
    Ok(HttpResponse::Ok()
        .insert_header(header::CacheControl(vec![
            header::CacheDirective::Private,
            header::CacheDirective::MaxAge(300u32),
        ]))
        .json(genres))
}

fn parse_sort(raw: Option<&str>, has_text_query: bool) -> Result<db::CatalogSort, ApiError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None if has_text_query => Ok(db::CatalogSort::Relevance),
        None => Ok(db::CatalogSort::Title),
        Some("relevance") => Ok(db::CatalogSort::Relevance),
        Some("title") => Ok(db::CatalogSort::Title),
        Some("year") => Ok(db::CatalogSort::Year),
        Some("releaseDate") => Ok(db::CatalogSort::ReleaseDate),
        Some("duration") => Ok(db::CatalogSort::Duration),
        Some(_) => Err(ApiError::BadRequest(
            "sort must be relevance, title, year, releaseDate, or duration".to_string(),
        )),
    }
}

fn parse_order(raw: Option<&str>) -> Result<bool, ApiError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None | Some("asc") => Ok(false),
        Some("desc") => Ok(true),
        Some(_) => Err(ApiError::BadRequest(
            "order must be asc or desc".to_string(),
        )),
    }
}

fn parse_genre(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|g| !g.is_empty())
        .map(String::from)
}

fn parse_item_type(raw: Option<&str>) -> Result<Option<String>, ApiError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(t @ ("movie" | "show")) => Ok(Some(t.to_string())),
        Some(_) => Err(ApiError::BadRequest(
            "type must be 'movie' or 'show'".to_string(),
        )),
    }
}

/// Search and browse the catalog.
///
/// Full-text search over movies and shows, optionally filtered by genre and type and sorted. At least one of "q" or "genre" is required.
#[utoipa::path(
    get,
    path = "/api/search",
    params(
        ("q" = Option<String>, Query, description = "Search terms (full-text; multiple words supported)"),
        ("genre" = Option<String>, Query, description = "Only return titles with this genre (exact name from /api/genres)"),
        ("type" = Option<String>, Query, description = "movie | show (default both)"),
        ("sort" = Option<String>, Query, description = "relevance | title | year | releaseDate | duration (default relevance when q is set, otherwise title)"),
        ("order" = Option<String>, Query, description = "asc | desc (default asc; ignored for relevance)"),
        ("limit" = Option<u32>, Query, description = "Max results to return"),
        ("offset" = Option<u32>, Query, description = "Result offset (for pagination/infinite scroll)")
    ),
    responses(
        (status = 200, description = "Paginated search results", body = SearchResultsPage),
        (status = 400, description = "Missing q/genre or invalid parameter")
    )
)]
#[get("/search")]
async fn search_handler(
    state: web::Data<AppState>,
    query: web::Query<CatalogSearchQuery>,
) -> Result<impl Responder, ApiError> {
    let limit = query.limit.unwrap_or(50) as i64;
    let offset = query.offset.unwrap_or(0) as i64;

    let fts_query = query.q.as_deref().and_then(db::build_fts_query);
    let genre = parse_genre(query.genre.as_deref());
    if fts_query.is_none() && genre.is_none() {
        return Err(ApiError::BadRequest(
            "At least one of q or genre is required".to_string(),
        ));
    }

    let sort = parse_sort(query.sort.as_deref(), fts_query.is_some())?;
    let descending = parse_order(query.order.as_deref())?;

    let filters = db::CatalogFilters {
        fts_query,
        genre,
        item_type: parse_item_type(query.item_type.as_deref())?,
        sort,
        descending,
        server_id: None,
        library_id: None,
    };
    let total = db::search_catalog_total(&state.db_pool, &filters).await?;
    let items = db::search_catalog(&state.db_pool, &filters, limit, offset).await?;
    Ok(HttpResponse::Ok().json(SearchResultsPage {
        total,
        limit,
        offset,
        items,
    }))
}

/// Get media details by GUID across all servers.
///
/// Aggregates the same title from every server (by GUID). Returns versions and availability per server.
#[utoipa::path(
    get,
    path = "/api/media/{guid}",
    params(("guid" = String, Path, description = "Media GUID (e.g. plex://movie/...)")),
    responses(
        (status = 200, description = "Media details", body = MediaDetails),
        (status = 404, description = "Media not found")
    )
)]
#[get("/media/{guid:.*}")]
async fn get_media_details_handler(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let guid = path.into_inner();
    match db::get_details_by_guid(&state.db_pool, &guid).await? {
        Some(details) => Ok(HttpResponse::Ok().json(details)),
        None => Err(ApiError::NotFound("Media not found".to_string())),
    }
}

/// List seasons for a show.
///
/// Returns seasons from the catalog. If the show has no seasons, may return a single virtual season with all episodes.
#[utoipa::path(
    get,
    path = "/api/servers/{server_id}/shows/{show_id}/seasons",
    params(
        ("server_id" = String, Path, description = "Server ID (Plex client identifier)"),
        ("show_id" = String, Path, description = "Show rating key (from library items or children)")
    ),
    responses(
        (status = 200, description = "List of seasons", body = Vec<SeasonSummary>),
        (status = 404, description = "Server or show not found")
    )
)]
#[get("/servers/{server_id}/shows/{show_id}/seasons")]
async fn get_seasons_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, show_id) = path.into_inner();
    let seasons = db::get_show_seasons(&state.db_pool, &show_id, &server_id).await?;
    Ok(HttpResponse::Ok().json(seasons))
}

/// List episodes for a season.
///
/// Returns episodes from the catalog with resolution and subtitle info. Season_id can be a season rating key or the show rating key when there are no seasons (like a mini-series)
#[utoipa::path(
    get,
    path = "/api/servers/{server_id}/seasons/{season_id}/episodes",
    params(
        ("server_id" = String, Path, description = "Server ID (Plex client identifier)"),
        ("season_id" = String, Path, description = "Season rating key, or show rating key if show has no seasons")
    ),
    responses(
        (status = 200, description = "List of episodes", body = Vec<EpisodeDetails>),
        (status = 404, description = "Server or season not found")
    )
)]
#[get("/servers/{server_id}/seasons/{season_id}/episodes")]
async fn get_episodes_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, season_id) = path.into_inner();
    let episodes = db::get_season_episodes(&state.db_pool, &season_id, &server_id).await?;
    Ok(HttpResponse::Ok().json(episodes))
}
