use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tracing::error;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("unauthorized")]
    Unauthorized,

    #[error("not found")]
    NotFound,

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("s3 error: {0}")]
    S3(String),

    #[error("crypto error: {0}")]
    Crypto(#[from] crate::crypto::CryptoError),

    #[error("compression error: {0}")]
    Compression(String),

    #[error("internal error: {0}")]
    Internal(String),
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".into()),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".into()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            other => {
                error!(%other, "internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".into(),
                )
            }
        };

        (status, axum::Json(ErrorBody { error: message })).into_response()
    }
}
