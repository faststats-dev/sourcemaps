use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use tower_http::trace::TraceLayer;
use tracing::info;
use uuid::Uuid;

use crate::SharedState;
use crate::auth::{AdminAuthenticatedProject, AuthenticatedProject};
use crate::error::AppError;
use crate::storage::StoredObjectMeta;

const INGEST_MAX_BODY_BYTES: usize = 50 * 1024 * 1024;

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPayload {
    #[serde(default)]
    pub excluded_build_ids: Vec<String>,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupResponse {
    pub ok: bool,
    pub latest_build_id: Option<String>,
    pub excluded_build_ids: Vec<String>,
    pub deleted_build_ids: Vec<String>,
    pub deleted_files: u64,
}

pub fn public_router(state: SharedState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route(
            "/api/sourcemaps",
            post(ingest).route_layer(DefaultBodyLimit::max(INGEST_MAX_BODY_BYTES)),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub fn internal_router(state: SharedState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/internal/sourcemaps", delete(wipe))
        .route("/internal/sourcemaps/cleanup", delete(cleanup_old_builds))
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
    }

    record_build_id(
        &state.db,
        project_id,
        &payload.build_id,
        &payload.uploaded_at,
    )
    .await?;

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

async fn record_build_id(
    db: &sqlx::PgPool,
    project_id: Uuid,
    build_id: &str,
    uploaded_at: &str,
) -> Result<(), AppError> {
    sqlx::query(
        r#"
        INSERT INTO project_build_ids (project_id, build_id, deployed_at)
        VALUES ($1, $2, ($3::timestamptz AT TIME ZONE 'UTC'))
        ON CONFLICT (project_id, build_id)
        DO UPDATE SET deployed_at = EXCLUDED.deployed_at
        "#,
    )
    .bind(project_id)
    .bind(build_id)
    .bind(uploaded_at)
    .execute(db)
    .await
    .map_err(AppError::Database)?;

    Ok(())
}

pub async fn wipe(
    auth: AdminAuthenticatedProject,
    State(state): State<SharedState>,
) -> Result<Json<WipeResponse>, AppError> {
    let project_id = auth.project_id;
    let prefix = format!("{project_id}/");

    let deleted_files = state.storage.delete_prefix(&prefix).await?;
    let deleted_rows = 0;

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
    let prefix = format!("{}/", auth.project_id);
    let keys = state.storage.list_prefix_keys(&prefix).await?;
    let mut sourcemaps: Vec<SourcemapListItem> = keys
        .into_iter()
        .filter_map(|key| {
            parse_sourcemap_key(&key).map(|(build_id, file_name)| SourcemapListItem {
                build_id,
                file_name,
            })
        })
        .collect();
    sourcemaps.sort_by(|a, b| {
        b.build_id
            .cmp(&a.build_id)
            .then_with(|| b.file_name.cmp(&a.file_name))
    });

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

pub async fn cleanup_old_builds(
    auth: AdminAuthenticatedProject,
    State(state): State<SharedState>,
    Json(payload): Json<CleanupPayload>,
) -> Result<Json<CleanupResponse>, AppError> {
    let project_id = auth.project_id;
    let prefix = format!("{project_id}/");
    let objects = state.storage.list_prefix_objects(&prefix).await?;

    let excluded_build_ids = normalized_build_ids(&payload.excluded_build_ids);
    let (latest_build_id, deleted_build_ids) =
        select_builds_for_cleanup(&objects, &excluded_build_ids);
    let deleted_build_ids_set: HashSet<&str> =
        deleted_build_ids.iter().map(String::as_str).collect();

    let keys_to_delete: Vec<String> = objects
        .into_iter()
        .filter(|object| {
            parse_sourcemap_key(&object.key)
                .map(|(build_id, _)| deleted_build_ids_set.contains(build_id.as_str()))
                .unwrap_or(false)
        })
        .map(|object| object.key)
        .collect();

    let deleted_files = state.storage.delete_keys(&keys_to_delete).await?;

    info!(
        %project_id,
        latest_build_id = ?latest_build_id,
        excluded_build_ids = ?excluded_build_ids,
        deleted_build_ids = ?deleted_build_ids,
        deleted_files,
        "cleaned up old sourcemap builds"
    );

    Ok(Json(CleanupResponse {
        ok: true,
        latest_build_id,
        excluded_build_ids,
        deleted_build_ids,
        deleted_files,
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
    require_non_empty("uploaded_at", &payload.uploaded_at)?;
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

fn normalized_build_ids(input: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for value in input {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn select_builds_for_cleanup(
    objects: &[StoredObjectMeta],
    excluded_build_ids: &[String],
) -> (Option<String>, Vec<String>) {
    let mut build_latest_modified: HashMap<String, i64> = HashMap::new();
    for object in objects {
        if let Some((build_id, _)) = parse_sourcemap_key(&object.key) {
            let modified = object.last_modified_epoch_seconds.unwrap_or(i64::MIN);
            build_latest_modified
                .entry(build_id)
                .and_modify(|current| *current = (*current).max(modified))
                .or_insert(modified);
        }
    }

    let latest_build_id = build_latest_modified
        .iter()
        .max_by(|(a_build, a_modified), (b_build, b_modified)| {
            a_modified
                .cmp(b_modified)
                .then_with(|| a_build.cmp(b_build))
        })
        .map(|(build_id, _)| build_id.clone());
    let excluded_set: HashSet<&str> = excluded_build_ids.iter().map(String::as_str).collect();
    let mut deleted_build_ids: Vec<String> = build_latest_modified
        .into_keys()
        .filter(|build_id| {
            Some(build_id) != latest_build_id.as_ref() && !excluded_set.contains(build_id.as_str())
        })
        .collect();
    deleted_build_ids.sort_unstable();

    (latest_build_id, deleted_build_ids)
}

#[cfg(test)]
mod tests {
    use super::{
        map_file_name, normalized_build_ids, parse_sourcemap_key, require_non_empty,
        select_builds_for_cleanup,
    };
    use crate::storage::StoredObjectMeta;

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

    #[test]
    fn normalized_build_ids_deduplicates_and_trims() {
        let normalized = normalized_build_ids(&[
            " build-1 ".to_string(),
            "build-1".to_string(),
            "".to_string(),
            "   ".to_string(),
            "build-2".to_string(),
        ]);
        assert_eq!(
            normalized,
            vec!["build-1".to_string(), "build-2".to_string()]
        );
    }

    #[test]
    fn select_builds_for_cleanup_keeps_latest_and_excluded() {
        let objects = vec![
            StoredObjectMeta {
                key: "proj/build-001/app.js.map".to_string(),
                last_modified_epoch_seconds: Some(100),
            },
            StoredObjectMeta {
                key: "proj/build-002/app.js.map".to_string(),
                last_modified_epoch_seconds: Some(200),
            },
            StoredObjectMeta {
                key: "proj/build-003/app.js.map".to_string(),
                last_modified_epoch_seconds: Some(400),
            },
            StoredObjectMeta {
                key: "proj/build-004/app.js.map".to_string(),
                last_modified_epoch_seconds: Some(300),
            },
        ];
        let excluded = vec!["build-002".to_string()];
        let (latest, deleted) = select_builds_for_cleanup(&objects, &excluded);

        assert_eq!(latest, Some("build-003".to_string()));
        assert_eq!(
            deleted,
            vec!["build-001".to_string(), "build-004".to_string()]
        );
    }

    #[test]
    fn select_builds_for_cleanup_handles_no_builds() {
        let (latest, deleted) = select_builds_for_cleanup(&[], &[]);
        assert_eq!(latest, None);
        assert!(deleted.is_empty());
    }

    #[test]
    fn select_builds_for_cleanup_uses_latest_object_within_build() {
        let objects = vec![
            StoredObjectMeta {
                key: "proj/build-a/a.js.map".to_string(),
                last_modified_epoch_seconds: Some(100),
            },
            StoredObjectMeta {
                key: "proj/build-a/b.js.map".to_string(),
                last_modified_epoch_seconds: Some(500),
            },
            StoredObjectMeta {
                key: "proj/build-b/a.js.map".to_string(),
                last_modified_epoch_seconds: Some(400),
            },
        ];

        let (latest, deleted) = select_builds_for_cleanup(&objects, &[]);
        assert_eq!(latest, Some("build-a".to_string()));
        assert_eq!(deleted, vec!["build-b".to_string()]);
    }
}
