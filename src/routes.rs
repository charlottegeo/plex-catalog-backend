use crate::auth::{CSHAuth, User};
use crate::{
    db,
    error::ApiError,
    models::{DbServer, ImageQuery, MediaRequestPayload, SearchQuery},
    AppState, SYNC_INTERVAL_MINUTES,
};
use actix_web::{
    delete, get, http::header, post, web, HttpMessage, HttpRequest, HttpResponse, Responder, Result,
};

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
            .service(search_handler)
            .service(discover_handler)
            .service(create_request_handler)
            .service(get_all_requests_handler)
            .service(delete_request_handler)
            .service(get_media_details_handler)
            .service(get_seasons_handler)
            .service(get_episodes_handler)
            .service(create_play_queue_handler),
    );
}

/// List all Plex servers known to the catalog.
///
/// Returns servers from the PostgreSQL database. Includes connection info and online status.
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
    let info = db::get_system_info(&state.db_pool, SYNC_INTERVAL_MINUTES).await?;
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
/// Returns movies and shows in the given library section. Use children or details endpoints to get more information.
#[utoipa::path(
    get,
    path = "/api/servers/{server_id}/libraries/{library_key}",
    params(
        ("server_id" = String, Path, description = "Server ID (Plex client identifier)"),
        ("library_key" = String, Path, description = "Library section key from /libraries")
    ),
    responses(
        (status = 200, description = "List of library items", body = Vec<Item>),
        (status = 404, description = "Server not found")
    )
)]
#[get("/servers/{server_id}/libraries/{library_key}")]
async fn get_library_items_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, library_key) = path.into_inner();
    let items = db::get_library_items(&state.db_pool, &server_id, &library_key).await?;
    Ok(HttpResponse::Ok().json(items))
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
/// Images are cached. Optional `width` and `height` query params trigger transcoding to that size.
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
    let image_response = client
        .get_image(
            &server_details.connection_uri,
            &server_details.access_token,
            &image_path,
            width,
            height,
        )
        .await?;

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

/// Search Plex's global discover catalog for movies and TV shows.
///
/// Uses the Plex account token to query metadata.provider.plex.tv.
#[utoipa::path(
    get,
    path = "/api/discover",
    params(("q" = String, Query, description = "Search query")),
    responses((status = 200, description = "Search results from Plex discover", body = serde_json::Value))
)]
#[get("/discover")]
async fn discover_handler(
    state: web::Data<AppState>,
    query: web::Query<SearchQuery>,
) -> Result<impl Responder, ApiError> {
    let results = state.plex_client.search_global_discover(&query.q).await?;

    Ok(HttpResponse::Ok().json(results))
}

/// Create or update a media request.
///
/// On conflict with an existing pending request for the same (username, guid), merges seasons and updates resolution.
#[utoipa::path(
    post,
    path = "/api/requests",
    request_body = MediaRequestPayload,
    responses((status = 200, description = "Created or updated request", body = MediaRequest))
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
        .ok_or_else(|| ApiError::Unauthorized("Authentication required".to_string()))?;
    let payload = payload.into_inner();
    let guid_for_check = payload
        .guid
        .strip_prefix("plex://")
        .unwrap_or(&payload.guid);
    let is_upgrade = db::item_exists_by_guid(&state.db_pool, guid_for_check).await?;
    let request =
        db::create_or_update_request(&state.db_pool, &username, &payload, is_upgrade).await?;

    Ok(HttpResponse::Ok().json(request))
}

/// List all media requests (both pending and fulfilled).
#[utoipa::path(
    get,
    path = "/api/requests",
    responses((status = 200, description = "List of all requests", body = Vec<MediaRequest>))
)]
#[get("/requests")]
async fn get_all_requests_handler(state: web::Data<AppState>) -> Result<impl Responder, ApiError> {
    let requests = db::get_all_requests(&state.db_pool).await?;
    Ok(HttpResponse::Ok().json(requests))
}

/// Delete the user's own pending request.
#[utoipa::path(
    delete,
    path = "/api/requests/{id}",
    params(("id" = i32, Path, description = "Request ID to delete")),
    responses(
        (status = 200, description = "Request deleted"),
        (status = 403, description = "Request not found or does not belong to user")
    )
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
        .ok_or_else(|| ApiError::Unauthorized("Authentication required".to_string()))?;
    let id = path.into_inner();
    let rows_affected = db::delete_request(&state.db_pool, id, &username).await?;
    if rows_affected == 0 {
        return Err(ApiError::NotFound(
            "Request not found or does not belong to you".to_string(),
        ));
    }
    Ok(HttpResponse::Ok().finish())
}

/// Full-text search over the catalog.
///
/// Searches movies and shows by title/summary. Returns matching items with server and metadata from the catalog.
#[utoipa::path(
    get,
    path = "/api/search",
    params(("q" = String, Query, description = "Search terms (full-text; multiple words supported)")),
    responses((status = 200, description = "Search results", body = Vec<SearchResult>))
)]
#[get("/search")]
async fn search_handler(
    state: web::Data<AppState>,
    query: web::Query<SearchQuery>,
) -> Result<impl Responder, ApiError> {
    let search_results = db::search_items(&state.db_pool, &query.q).await?;
    Ok(HttpResponse::Ok().json(search_results))
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
        (status = 404, description = "Server not found")
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

/// Create a play queue for instant playback.
///
/// Calls the Plex playQueues API. Send `X-Plex-Client-Identifier` to use a unique session and avoid queue collisions between users.
#[utoipa::path(
    post,
    path = "/api/servers/{server_id}/play/{rating_key}",
    params(
        ("server_id" = String, Path, description = "Server ID (Plex client identifier)"),
        ("rating_key" = String, Path, description = "Item rating key to play (movie or episode)")
    ),
    responses(
        (status = 200, description = "Play queue created", body = PlayQueueResponse),
        (status = 404, description = "Server not found")
    )
)]
#[post("/servers/{server_id}/play/{rating_key}")]
async fn create_play_queue_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
    req: actix_web::HttpRequest,
) -> Result<impl Responder, ApiError> {
    let (server_id, rating_key) = path.into_inner();
    let client = &state.plex_client;
    let server_details = get_server_details_or_404(&state.db_pool, &server_id).await?;

    let base = server_details.connection_uri.trim_end_matches('/');
    let item_uri = format!("{}/library/metadata/{}", base, rating_key);

    let client_identifier = req
        .headers()
        .get("X-Plex-Client-Identifier")
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    let client_id = client_identifier.as_deref();
    let container = client
        .create_play_queue(
            &server_details.connection_uri,
            &server_details.access_token,
            &item_uri,
            client_id,
        )
        .await?;

    Ok(HttpResponse::Ok().json(container.media_container))
}

/// List episodes for a season.
///
/// Returns episodes from the catalog with version and subtitle info. Season_id can be a season rating key or the show rating key when there are no seasons.
#[utoipa::path(
    get,
    path = "/api/servers/{server_id}/seasons/{season_id}/episodes",
    params(
        ("server_id" = String, Path, description = "Server ID (Plex client identifier)"),
        ("season_id" = String, Path, description = "Season rating key, or show rating key if show has no seasons")
    ),
    responses(
        (status = 200, description = "List of episodes", body = Vec<EpisodeDetails>),
        (status = 404, description = "Server not found")
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
