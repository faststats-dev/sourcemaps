use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use tower_http::trace::TraceLayer;
use tracing::info;
use uuid::Uuid;

use crate::SharedState;
use crate::auth::{AdminAuthenticatedProject, AuthenticatedProject};
use crate::error::AppError;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcemapEntry {
    pub file_name: String,
    pub sourcemap: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestPayload {
    pub build_id: String,
    pub bundler: String,
    pub uploaded_at: String,
    pub sourcemaps: Vec<SourcemapEntry>,
}

#[derive(Serialize)]
pub struct IngestResponse {
    pub ok: bool,
    pub ingested_count: usize,
}

#[derive(Serialize)]
pub struct WipeResponse {
    pub ok: bool,
    pub deleted_files: u64,
    pub deleted_rows: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcemapListItem {
    pub build_id: String,
    pub file_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcemapListResponse {
    pub ok: bool,
    pub sourcemaps: Vec<SourcemapListItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPayload {
    pub build_id: String,
    pub file_name: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginalPosition {
    pub source: String,
    pub line: u32,
    pub column: u32,
    pub name: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResponse {
    pub ok: bool,
    pub original: OriginalPosition,
}

pub fn public_router(state: SharedState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/api/sourcemaps", post(ingest))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub fn internal_router(state: SharedState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/internal/sourcemaps", delete(wipe))
        .route("/internal/sourcemaps", get(list_sourcemaps))
        .route("/internal/sourcemaps/apply", post(apply_sourcemap))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

fn s3_key(project_id: Uuid, build_id: &str, file_name: &str) -> String {
    format!("{project_id}/{build_id}/{file_name}")
}

pub async fn health() -> &'static str {
    "ok"
}

pub async fn ingest(
    auth: AuthenticatedProject,
    State(state): State<SharedState>,
    Json(payload): Json<IngestPayload>,
) -> Result<(StatusCode, Json<IngestResponse>), AppError> {
    validate_ingest_payload(&payload)?;

    let project_id = auth.project_id;

    for entry in &payload.sourcemaps {
        let key = s3_key(project_id, &payload.build_id, &entry.file_name);

        state.storage.put(&key, entry.sourcemap.as_bytes()).await?;

        sqlx::query(
            "INSERT INTO source_maps (project_id, type, file, url, created_at) VALUES ($1, 'file', $2, NULL, NOW())",
        )
        .bind(project_id)
        .bind(&key)
        .execute(&state.db)
        .await?;
    }

    let ingested = payload.sourcemaps.len();
    let total_bytes: usize = payload.sourcemaps.iter().map(|e| e.sourcemap.len()).sum();
    let file_names: Vec<&str> = payload
        .sourcemaps
        .iter()
        .map(|e| e.file_name.as_str())
        .collect();

    info!(
        %project_id,
        build_id = %payload.build_id,
        bundler = %payload.bundler,
        uploaded_at = %payload.uploaded_at,
        count = ingested,
        total_bytes,
        files = ?file_names,
        "ingested sourcemaps"
    );

    Ok((
        StatusCode::OK,
        Json(IngestResponse {
            ok: true,
            ingested_count: ingested,
        }),
    ))
}

pub async fn wipe(
    auth: AdminAuthenticatedProject,
    State(state): State<SharedState>,
) -> Result<Json<WipeResponse>, AppError> {
    let project_id = auth.project_id;
    let prefix = format!("{project_id}/");

    let deleted_files = state.storage.delete_prefix(&prefix).await?;

    let result = sqlx::query("DELETE FROM source_maps WHERE project_id = $1")
        .bind(project_id)
        .execute(&state.db)
        .await?;

    let deleted_rows = result.rows_affected();

    info!(%project_id, deleted_files, deleted_rows, "wiped all sourcemaps");

    Ok(Json(WipeResponse {
        ok: true,
        deleted_files,
        deleted_rows,
    }))
}

pub async fn list_sourcemaps(
    auth: AdminAuthenticatedProject,
    State(state): State<SharedState>,
) -> Result<Json<SourcemapListResponse>, AppError> {
    let rows =
        sqlx::query("SELECT file FROM source_maps WHERE project_id = $1 ORDER BY created_at DESC")
            .bind(auth.project_id)
            .fetch_all(&state.db)
            .await?;

    let sourcemaps = rows
        .into_iter()
        .filter_map(|row| {
            let key: String = row.get("file");
            parse_sourcemap_key(&key).map(|(build_id, file_name)| SourcemapListItem {
                build_id,
                file_name,
            })
        })
        .collect();

    Ok(Json(SourcemapListResponse {
        ok: true,
        sourcemaps,
    }))
}

pub async fn apply_sourcemap(
    auth: AdminAuthenticatedProject,
    State(state): State<SharedState>,
    Json(payload): Json<ApplyPayload>,
) -> Result<Json<ApplyResponse>, AppError> {
    require_non_empty("build_id", &payload.build_id)?;
    require_non_empty("file_name", &payload.file_name)?;

    let map_file = map_file_name(&payload.file_name);
    let key = s3_key(auth.project_id, &payload.build_id, &map_file);
    let data = state.storage.get(&key).await?;
    let source_map = sourcemap::SourceMap::from_slice(&data)
        .map_err(|e| AppError::BadRequest(format!("invalid sourcemap: {e}")))?;
    let token = source_map
        .lookup_token(
            payload.line.saturating_sub(1),
            payload.column.saturating_sub(1),
        )
        .ok_or(AppError::NotFound)?;
    let source = token.get_source().ok_or(AppError::NotFound)?;
    let src_line = token.get_src_line();
    let src_col = token.get_src_col();

    if src_line == u32::MAX || src_col == u32::MAX {
        return Err(AppError::NotFound);
    }

    Ok(Json(ApplyResponse {
        ok: true,
        original: OriginalPosition {
            source: source.to_string(),
            line: src_line.saturating_add(1),
            column: src_col.saturating_add(1),
            name: token.get_name().map(ToString::to_string),
        },
    }))
}

fn map_file_name(file_name: &str) -> String {
    if file_name.ends_with(".map") {
        file_name.to_string()
    } else {
        format!("{file_name}.map")
    }
}

fn require_non_empty(field: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() {
        return Err(AppError::BadRequest(format!("{field} is required")));
    }
    Ok(())
}

fn validate_ingest_payload(payload: &IngestPayload) -> Result<(), AppError> {
    require_non_empty("build_id", &payload.build_id)?;
    if payload.sourcemaps.is_empty() {
        return Err(AppError::BadRequest("no sourcemaps provided".into()));
    }

    for entry in &payload.sourcemaps {
        require_non_empty("file_name", &entry.file_name)?;
        require_non_empty("sourcemap", &entry.sourcemap)?;
    }

    Ok(())
}

fn parse_sourcemap_key(key: &str) -> Option<(String, String)> {
    let mut parts = key.splitn(3, '/');
    let _project_id = parts.next()?;
    let build_id = parts.next()?.to_string();
    let file_name = parts.next()?.to_string();
    Some((build_id, file_name))
}

#[cfg(test)]
mod tests {
    use super::{map_file_name, parse_sourcemap_key, require_non_empty};

    #[test]
    fn map_file_name_adds_map_suffix_when_missing() {
        assert_eq!(map_file_name("app.js"), "app.js.map");
        assert_eq!(map_file_name("bundle.js.map"), "bundle.js.map");
    }

    #[test]
    fn parse_sourcemap_key_extracts_build_and_file_name() {
        let parsed = parse_sourcemap_key("proj-id/build-42/chunk.js.map");
        assert_eq!(
            parsed,
            Some(("build-42".to_string(), "chunk.js.map".to_string()))
        );
    }

    #[test]
    fn require_non_empty_rejects_whitespace() {
        let err = require_non_empty("build_id", "   ").expect_err("value should be invalid");
        assert!(format!("{err}").contains("build_id is required"));
    }
}
