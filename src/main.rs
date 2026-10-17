use crate::models::{CachedImage, GenreCount};
use crate::plex_client::PlexClient;
use actix_web::{web, App, HttpServer};
use moka::future::Cache;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

mod auth;
mod db;
mod error;
mod models;
mod oidc_client;
mod pings;
mod plex_client;
mod resolution;
mod routes;
mod server_status;
mod sync;

/// Adds Bearer (JWT) security scheme for Swagger UI.
struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let bearer = SecurityScheme::Http(
            HttpBuilder::new()
                .scheme(HttpAuthScheme::Bearer)
                .bearer_format("JWT")
                .description(Some("Keycloak JWT token, use CSH SSO login to get token."))
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
        routes::get_genres_handler,
        routes::discover_handler,
        routes::create_request_handler,
        routes::get_all_requests_handler,
        routes::get_active_requests_by_guid_handler,
        routes::delete_request_handler,
        routes::get_media_details_handler,
        routes::get_seasons_handler,
        routes::get_episodes_handler,
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
        crate::models::SearchResultsPage,
        crate::models::SearchQuery,
        crate::models::CatalogSearchQuery,
        crate::models::GenreCount,
        crate::models::GenresQuery,
        crate::models::Tag,
        crate::models::DiscoverResultsPage,
        crate::models::LibraryItemsQuery,
        crate::models::LibraryItemsPage,
        crate::models::MediaRequestPayload,
        crate::models::MediaRequest,
        crate::models::RequestsQuery,
        crate::models::ImageQuery,
        crate::models::MediaDetails,
        crate::models::MediaVersion,
        crate::models::ServerAvailability,
        crate::models::SeasonSummary,
        crate::models::EpisodeDetails,
    )),
    modifiers(&SecurityAddon),
    info(
        title = "Plex Catalog Backend API",
        version = "1.0.0",
        description = "API for browsing a media catalog populated from Plex servers"
    )
)]
struct ApiDoc;

#[derive(Clone)]
pub struct AppState {
    pub plex_client: PlexClient,
    pub db_pool: sqlx::PgPool,
    pub image_cache: Cache<String, CachedImage>,
    pub genre_cache: Cache<String, Vec<GenreCount>>,
    pub sync_semaphore: Arc<Semaphore>,
    pub oidc_client: oidc_client::OidcClient,
    pub ping_client: pings::PingClient,
    pub sync_interval_minutes: u64,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    tracing::info!("Backend starting!");

    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let db_pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .expect("Failed to connect to Postgres.");

    tracing::info!("Postgres connected!");

    if let Err(e) = sqlx::migrate!("./migrations").run(&db_pool).await {
        tracing::warn!(error = ?e, "Migration failed at startup; continuing...");
    } else {
        tracing::info!("Migrations OK.");
    }

    let oidc_client = oidc_client::OidcClient::new();

    let sync_interval_minutes = std::env::var("SYNC_INTERVAL_MINUTES")
        .unwrap_or_else(|_| "90".to_string())
        .parse()
        .unwrap_or(90);

    let pings_token = std::env::var("PINGS_TOKEN").unwrap_or_default();
    let pings_request_route = std::env::var("PINGS_REQUEST_ROUTE").unwrap_or_default();
    let pings_status_route = std::env::var("PINGS_STATUS_ROUTE").unwrap_or_default();

    let ping_client = pings::PingClient::new(pings_token, pings_request_route, pings_status_route)
        .expect("Failed to initialize PingClient");

    let app_state = web::Data::new(AppState {
        plex_client: PlexClient::new(),
        db_pool: db_pool.clone(),
        image_cache: Cache::builder()
            .max_capacity(256 * 1024 * 1024)
            .weigher(|_key, value: &CachedImage| -> u32 { value.bytes.len() as u32 })
            .time_to_live(Duration::from_secs(12 * 60 * 60))
            .build(),
        genre_cache: Cache::builder()
            .max_capacity(3)
            .time_to_live(Duration::from_secs(5 * 60))
            .build(),
        sync_semaphore: Arc::new(Semaphore::new(5)),
        oidc_client,
        ping_client,
        sync_interval_minutes,
    });

    tokio::spawn(sync::database_sync_scheduler(app_state.clone()));
    tracing::info!("HTTP listening on 0.0.0.0:3001");
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
