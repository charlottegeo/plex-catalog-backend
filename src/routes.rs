use crate::auth::CSHAuth;
use crate::{db, error::ApiError, models::SearchQuery, AppState};
use actix_web::{get, http::header, web, HttpResponse, Responder, Result};

async fn get_server_details_or_404(
    db_pool: &sqlx::PgPool,
    server_id: &str,
) -> Result<crate::models::DbServer, ApiError> {
    db::get_server_details(db_pool, server_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Server not found in database".to_string()))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/api")
            .wrap(CSHAuth::enabled())
            .service(get_servers_handler)
            .service(get_libraries_handler)
            .service(get_library_items_handler)
            .service(get_item_details_handler)
            .service(get_item_children_handler)
            .service(get_image_handler)
            .service(search_handler)
            .service(get_media_details_handler)
            .service(get_seasons_handler)
            .service(get_episodes_handler),
    );
}

#[get("/servers")]
async fn get_servers_handler(state: web::Data<AppState>) -> Result<impl Responder, ApiError> {
    let servers = db::get_all_servers(&state.db_pool).await?;
    Ok(HttpResponse::Ok().json(servers))
}

#[get("/servers/{server_id}/libraries")]
async fn get_libraries_handler(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let server_id = path.into_inner();
    let libraries = db::get_server_libraries(&state.db_pool, &server_id).await?;
    Ok(HttpResponse::Ok().json(libraries))
}

#[get("/servers/{server_id}/libraries/{library_key}")]
async fn get_library_items_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, library_key) = path.into_inner();
    let items = db::get_library_items(&state.db_pool, &server_id, &library_key).await?;
    Ok(HttpResponse::Ok().json(items))
}

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

#[get("/servers/{server_id}/image/{image_path:.*}")]
async fn get_image_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, image_path) = path.into_inner();
    let cache_key = format!("{}/{}", server_id, image_path);

    if let Some(image_bytes) = state.image_cache.get(&cache_key).await {
        return Ok(HttpResponse::Ok()
            .content_type("image/jpeg")
            .insert_header(header::CacheControl(vec![header::CacheDirective::MaxAge(
                3600u32,
            )]))
            .body(image_bytes));
    }

    let server_details = get_server_details_or_404(&state.db_pool, &server_id).await?;
    let client = &state.plex_client;
    let image_response = client
        .get_image(
            &server_details.connection_uri,
            &server_details.access_token,
            &image_path,
        )
        .await?;

    let content_type = image_response
        .headers()
        .get("content-type")
        .and_then(|val| val.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let image_bytes = image_response.bytes().await.map_err(ApiError::from)?;

    state
        .image_cache
        .insert(cache_key, image_bytes.clone())
        .await;

    Ok(HttpResponse::Ok()
        .content_type(content_type)
        .insert_header(header::CacheControl(vec![header::CacheDirective::MaxAge(
            3600u32,
        )]))
        .body(image_bytes))
}

#[get("/search")]
async fn search_handler(
    state: web::Data<AppState>,
    query: web::Query<SearchQuery>,
) -> Result<impl Responder, ApiError> {
    let search_results = db::search_items(&state.db_pool, &query.q).await?;
    Ok(HttpResponse::Ok().json(search_results))
}

#[get("/media/{guid}")]
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

#[get("/servers/{server_id}/shows/{show_id}/seasons")]
async fn get_seasons_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, show_id) = path.into_inner();
    let seasons = db::get_show_seasons(&state.db_pool, &show_id, &server_id).await?;
    Ok(HttpResponse::Ok().json(seasons))
}

#[get("/servers/{server_id}/seasons/{season_id}/episodes")]
async fn get_episodes_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, season_id) = path.into_inner();
    let episodes = db::get_season_episodes(&state.db_pool, &season_id, &server_id).await?;
    Ok(HttpResponse::Ok().json(episodes))
}
