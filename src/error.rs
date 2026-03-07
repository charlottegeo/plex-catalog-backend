use actix_web::{http::StatusCode, HttpResponse, ResponseError};
use std::fmt;

#[derive(Debug)]
pub enum ApiError {
    PlexRequestFailed(reqwest::Error),
    DbError(sqlx::Error),
    NotFound(String),
}

impl From<reqwest::Error> for ApiError {
    fn from(err: reqwest::Error) -> Self {
        ApiError::PlexRequestFailed(err)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        ApiError::DbError(err)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::PlexRequestFailed(e) => write!(f, "Failed to communicate with Plex: {}", e),
            ApiError::DbError(e) => write!(f, "Database error occurred: {}", e),
            ApiError::NotFound(msg) => write!(f, "{}", msg),
        }
    }
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            ApiError::PlexRequestFailed(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::DbError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
        }
    }

    fn error_response(&self) -> HttpResponse {
        HttpResponse::build(self.status_code()).body(self.to_string())
    }
}
