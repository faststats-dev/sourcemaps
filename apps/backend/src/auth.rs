use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::request::Parts;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use sqlx::Row;
use uuid::Uuid;

use crate::SharedState;
use crate::crypto::Crypto;
use crate::error::AppError;

const HEADER_AUTHORIZATION: &str = "authorization";
const HEADER_ADMIN_TOKEN: &str = "x-admin-token";
const HEADER_PROJECT_ID: &str = "x-project-id";
const BEARER_PREFIX: &str = "Bearer ";

pub struct AuthenticatedProject {
    pub project_id: Uuid,
}

pub struct AdminAuthenticatedProject {
    pub project_id: Uuid,
}

impl FromRequestParts<SharedState> for AuthenticatedProject {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &SharedState,
    ) -> Result<Self, Self::Rejection> {
        let token = bearer_token(&parts.headers)?;

        let prefix = extract_prefix(token).ok_or(AppError::Unauthorized)?;

        let row = sqlx::query(
            "SELECT project_id, encrypted_key FROM sourcemap_api_keys WHERE key_prefix = $1",
        )
        .bind(prefix)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::Unauthorized)?;

        let project_id: Uuid = row.get("project_id");
        let encrypted_key: String = row.get("encrypted_key");

        if !verify_api_key(&state.crypto, &encrypted_key, token)? {
            return Err(AppError::Unauthorized);
        }

        sqlx::query("UPDATE sourcemap_api_keys SET last_used_at = NOW() WHERE key_prefix = $1")
            .bind(prefix)
            .execute(&state.db)
            .await?;

        Ok(Self { project_id })
    }
}

impl FromRequestParts<SharedState> for AdminAuthenticatedProject {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &SharedState,
    ) -> Result<Self, Self::Rejection> {
        let admin_token = header_value(&parts.headers, HEADER_ADMIN_TOKEN)?;

        if admin_token != state.admin_token.as_ref() {
            return Err(AppError::Unauthorized);
        }

        let project_id = Uuid::parse_str(header_value(&parts.headers, HEADER_PROJECT_ID)?)
            .map_err(|_| AppError::Unauthorized)?;

        Ok(Self { project_id })
    }
}

fn header_value<'a>(headers: &'a HeaderMap, key: &str) -> Result<&'a str, AppError> {
    headers
        .get(key)
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)
}

fn bearer_token<'a>(headers: &'a HeaderMap) -> Result<&'a str, AppError> {
    header_value(headers, HEADER_AUTHORIZATION)?
        .strip_prefix(BEARER_PREFIX)
        .ok_or(AppError::Unauthorized)
}

fn extract_prefix(token: &str) -> Option<&str> {
    // Keys are `fsm_<hex>`, prefix is first 12 chars (e.g. `fsm_abcd1234`)
    if token.len() < 12 {
        return None;
    }
    Some(&token[..12])
}

fn verify_api_key(
    crypto: &Arc<Crypto>,
    encrypted_b64: &str,
    provided: &str,
) -> Result<bool, AppError> {
    let encrypted = BASE64
        .decode(encrypted_b64)
        .map_err(|_| AppError::Internal("invalid base64 in encrypted_key".into()))?;
    let decrypted = crypto.decrypt(&encrypted)?;
    let stored = std::str::from_utf8(&decrypted)
        .map_err(|_| AppError::Internal("invalid utf-8 in decrypted key".into()))?;
    Ok(stored == provided)
}
