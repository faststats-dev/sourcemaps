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
use crate::mappings::{require_non_empty, s3_key};
use crate::storage::StoredObjectMeta;

const INGEST_MAX_BODY_BYTES: usize = 50 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcemapEntry {
    pub file_name: String,
    pub sourcemap: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "mappingType", rename_all = "camelCase")]
pub enum IngestPayload {
    #[serde(rename = "javascript")]
    JavaScript {
        build_id: String,
        bundler: String,
        uploaded_at: String,
        sourcemaps: Vec<SourcemapEntry>,
    },
    #[serde(rename = "proguard")]
    Proguard {
        build_id: String,
        uploaded_at: String,
        mapping: String,
    },
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
#[serde(tag = "mappingType", rename_all = "camelCase")]
pub enum ApplyPayload {
    #[serde(rename = "javascript")]
    JavaScript {
        build_id: String,
        file_name: String,
        line: u32,
        column: u32,
    },
    #[serde(rename = "proguard")]
    Proguard {
        build_id: String,
        class_name: String,
        method_name: Option<String>,
        line: Option<u32>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPayload {
    #[serde(default)]
    pub excluded_build_ids: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResponse {
    pub ok: bool,
    pub original: crate::mappings::OriginalPosition,
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

pub async fn health() -> &'static str {
    "ok"
}

pub async fn ingest(
    auth: AuthenticatedProject,
    State(state): State<SharedState>,
    Json(payload): Json<IngestPayload>,
) -> Result<(StatusCode, Json<IngestResponse>), AppError> {
    let project_id = auth.project_id;

    let (build_id, uploaded_at, ingested, total_bytes, mapping_type) = match &payload {
        IngestPayload::JavaScript {
            build_id,
            bundler: _,
            uploaded_at,
            sourcemaps,
        } => {
            validate_js_ingest(build_id, uploaded_at, sourcemaps)?;
            let entries: Vec<(String, String)> = sourcemaps
                .iter()
                .map(|e| (e.file_name.clone(), e.sourcemap.clone()))
                .collect();
            crate::mappings::javascript::ingest(&state.storage, project_id, build_id, &entries)
                .await?;
            let total_bytes: usize = sourcemaps.iter().map(|e| e.sourcemap.len()).sum();
            (
                build_id.as_str(),
                uploaded_at.as_str(),
                sourcemaps.len(),
                total_bytes,
                "javascript",
            )
        }
        IngestPayload::Proguard {
            build_id,
            uploaded_at,
            mapping,
        } => {
            require_non_empty("build_id", build_id)?;
            require_non_empty("uploaded_at", uploaded_at)?;
            require_non_empty("mapping", mapping)?;
            crate::mappings::proguard::ingest(&state.storage, project_id, build_id, mapping)
                .await?;
            let total_bytes = mapping.len();
            (
                build_id.as_str(),
                uploaded_at.as_str(),
                1,
                total_bytes,
                "proguard",
            )
        }
    };

    record_build_id(&state.db, project_id, build_id, uploaded_at).await?;

    info!(
        %project_id,
        build_id,
        mapping_type,
        count = ingested,
        total_bytes,
        "ingested mappings"
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
    let original = match &payload {
        ApplyPayload::JavaScript {
            build_id,
            file_name,
            line,
            column,
        } => {
            require_non_empty("build_id", build_id)?;
            require_non_empty("file_name", file_name)?;
            let map_file = crate::mappings::javascript::map_file_name(file_name);
            let key = s3_key(auth.project_id, build_id, &map_file);
            let data = state.storage.get(&key).await?;
            crate::mappings::javascript::apply(&data, file_name, *line, *column)?
        }
        ApplyPayload::Proguard {
            build_id,
            class_name,
            method_name,
            line,
        } => {
            require_non_empty("build_id", build_id)?;
            require_non_empty("class_name", class_name)?;
            let key = crate::mappings::proguard::proguard_s3_key(auth.project_id, build_id);
            let data = state.storage.get(&key).await?;
            crate::mappings::proguard::apply(&data, class_name, method_name.as_deref(), *line)?
        }
    };

    Ok(Json(ApplyResponse { ok: true, original }))
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

fn validate_js_ingest(
    build_id: &str,
    uploaded_at: &str,
    sourcemaps: &[SourcemapEntry],
) -> Result<(), AppError> {
    require_non_empty("build_id", build_id)?;
    require_non_empty("uploaded_at", uploaded_at)?;
    if sourcemaps.is_empty() {
        return Err(AppError::BadRequest("no sourcemaps provided".into()));
    }
    for entry in sourcemaps {
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
    use super::{normalized_build_ids, parse_sourcemap_key, select_builds_for_cleanup};
    use crate::mappings::{javascript::map_file_name, require_non_empty};
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
