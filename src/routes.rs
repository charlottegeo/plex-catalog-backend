use crate::{error::ApiError, plex_client::PlexClient, AppState};
use actix_web::{get, web, HttpResponse, Responder, Result};
use futures::StreamExt;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/api")
            .service(get_servers_handler)
            .service(get_libraries_handler)
            .service(get_library_items_handler)
            .service(get_item_details_handler)
            .service(get_item_children_handler)
            .service(get_image_handler),
    );
}

async fn find_server_info(
    client: &mut PlexClient,
    server_id: &str,
) -> Result<(String, String), ApiError> {
    let servers = client.get_servers().await?;
    let target_server = servers
        .iter()
        .find(|s| s.client_identifier == server_id)
        .ok_or_else(|| ApiError::NotFound(format!("Server with ID {} not found", server_id)))?;
    let connection = target_server
        .connections
        .iter()
        .find(|c| !c.local)
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "Server '{}' has no remote connection",
                target_server.name
            ))
        })?;
    let server_token = target_server.access_token.as_ref().ok_or_else(|| {
        ApiError::NotFound(format!(
            "Server '{}' has no access token",
            target_server.name
        ))
    })?;
    Ok((connection.uri.clone(), server_token.clone()))
}

#[get("/servers")]
async fn get_servers_handler(state: web::Data<AppState>) -> Result<impl Responder, ApiError> {
    let mut client = state.plex_client.lock().await;
    client.ensure_logged_in().await?;
    let servers = client.get_servers().await?;
    Ok(HttpResponse::Ok().json(servers))
}

#[get("/servers/{server_id}/libraries")]
async fn get_libraries_handler(
    state: web::Data<AppState>,
    path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    let server_id = path.into_inner();
    let mut client = state.plex_client.lock().await;
    client.ensure_logged_in().await?;
    let (uri, token) = find_server_info(&mut client, &server_id).await?;
    let libraries = client.get_libraries(&uri, &token).await?;
    Ok(HttpResponse::Ok().json(libraries))
}

#[get("/servers/{server_id}/libraries/{library_key}")]
async fn get_library_items_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, library_key) = path.into_inner();
    let mut client = state.plex_client.lock().await;
    client.ensure_logged_in().await?;
    let (uri, token) = find_server_info(&mut client, &server_id).await?;
    let items = client.get_library_items(&uri, &token, &library_key).await?;
    Ok(HttpResponse::Ok().json(items))
}

#[get("/servers/{server_id}/items/{rating_key}")]
async fn get_item_details_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, rating_key) = path.into_inner();
    let mut client = state.plex_client.lock().await;
    client.ensure_logged_in().await?;
    let (uri, token) = find_server_info(&mut client, &server_id).await?;
    let item_option = client.get_item_details(&uri, &token, &rating_key).await?;

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
    let mut client = state.plex_client.lock().await;
    client.ensure_logged_in().await?;
    let (uri, token) = find_server_info(&mut client, &server_id).await?;
    let children = client.get_item_children(&uri, &token, &rating_key).await?;
    Ok(HttpResponse::Ok().json(children))
}

#[get("/servers/{server_id}/image/{image_path:.*}")]
async fn get_image_handler(
    state: web::Data<AppState>,
    path: web::Path<(String, String)>,
) -> Result<impl Responder, ApiError> {
    let (server_id, image_path) = path.into_inner();
    let mut client = state.plex_client.lock().await;
    client.ensure_logged_in().await?;
    let (uri, token) = find_server_info(&mut client, &server_id).await?;
    let image_response = client.get_image(&uri, &token, &image_path).await?;

    let content_type = image_response
        .headers()
        .get("content-type")
        .and_then(|val| val.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let body_stream = image_response
        .bytes_stream()
        .map(|res| res.map_err(|e| actix_web::error::ErrorInternalServerError(e)));

    Ok(HttpResponse::Ok()
        .content_type(content_type)
        .streaming(body_stream))
}
