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
pub struct UploadFile {
    pub file_name: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum IngestPayload {
    #[serde(rename = "javascript", rename_all = "camelCase")]
    JavaScript {
        build_id: String,
        uploaded_at: String,
        files: Vec<UploadFile>,
    },
    #[serde(rename = "proguard", rename_all = "camelCase")]
    Proguard {
        build_id: String,
        uploaded_at: String,
        files: Vec<UploadFile>,
    },
}

#[derive(Serialize)]
pub struct IngestResponse {
    pub ok: bool,
    pub ingested_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WipeResponse {
    pub ok: bool,
    pub deleted_files: u64,
    pub deleted_rows: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSourcemapPayload {
    pub s3_key: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSourcemapResponse {
    pub ok: bool,
    pub deleted_files: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcemapListItem {
    pub build_id: String,
    pub file_name: String,
    pub size: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcemapListResponse {
    pub ok: bool,
    pub sourcemaps: Vec<SourcemapListItem>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "mappingType")]
pub enum ApplyPayload {
    #[serde(rename = "javascript")]
    JavaScript {
        #[serde(alias = "buildId")]
        build_id: String,
        #[serde(alias = "fileName")]
        file_name: String,
        line: u32,
        column: u32,
    },
    #[serde(rename = "proguard")]
    Proguard {
        #[serde(alias = "buildId")]
        build_id: String,
        stacktrace: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct CleanupPayload {
    #[serde(default)]
    #[serde(alias = "excludedBuildIds")]
    pub excluded_build_ids: Vec<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ApplyResponse {
    JavaScript {
        ok: bool,
        original: crate::mappings::OriginalPosition,
    },
    Proguard {
        ok: bool,
        stacktrace: String,
    },
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
            "/v0/upload",
            post(ingest).route_layer(DefaultBodyLimit::max(INGEST_MAX_BODY_BYTES)),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub fn internal_router(state: SharedState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/internal/sourcemaps", delete(wipe))
        .route("/internal/sourcemaps/object", delete(delete_sourcemap))
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
            uploaded_at,
            files,
        } => {
            let entries = normalize_upload_files(build_id, uploaded_at, files)?;
            crate::mappings::javascript::ingest(&state.storage, project_id, build_id, &entries)
                .await?;
            let total_bytes: usize = entries.iter().map(|(_, content)| content.len()).sum();
            let file_names: Vec<&str> = entries
                .iter()
                .map(|(file_name, _)| file_name.as_str())
                .collect();
            info!(
                %project_id,
                build_id,
                files = ?file_names,
            );
            (
                build_id.as_str(),
                uploaded_at.as_str(),
                entries.len(),
                total_bytes,
                "javascript",
            )
        }
        IngestPayload::Proguard {
            build_id,
            uploaded_at,
            files,
        } => {
            let mappings = normalize_upload_files(build_id, uploaded_at, files)?;
            crate::mappings::proguard::ingest(&state.storage, project_id, build_id, &mappings)
                .await?;
            let total_bytes: usize = mappings.iter().map(|(_, mapping)| mapping.len()).sum();
            let file_names: Vec<&str> = mappings
                .iter()
                .map(|(file_name, _)| file_name.as_str())
                .collect();
            info!(
                %project_id,
                build_id,
                files = ?file_names,
            );
            (
                build_id.as_str(),
                uploaded_at.as_str(),
                mappings.len(),
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

pub async fn delete_sourcemap(
    auth: AdminAuthenticatedProject,
    State(state): State<SharedState>,
    Json(payload): Json<DeleteSourcemapPayload>,
) -> Result<Json<DeleteSourcemapResponse>, AppError> {
    let project_id = auth.project_id;
    let s3_key = validate_project_sourcemap_key(project_id, &payload.s3_key)?;
    let deleted_files = state
        .storage
        .delete_keys(std::slice::from_ref(&s3_key))
        .await?;

    info!(%project_id, s3_key, deleted_files, "deleted sourcemap");

    Ok(Json(DeleteSourcemapResponse {
        ok: true,
        deleted_files,
    }))
}

pub async fn list_sourcemaps(
    auth: AdminAuthenticatedProject,
    State(state): State<SharedState>,
) -> Result<Json<SourcemapListResponse>, AppError> {
    let prefix = format!("{}/", auth.project_id);
    let objects = state.storage.list_prefix_objects(&prefix).await?;
    let mut sourcemaps: Vec<SourcemapListItem> = objects
        .into_iter()
        .filter_map(|object| {
            parse_sourcemap_key(&object.key).map(|(build_id, file_name)| SourcemapListItem {
                build_id,
                file_name,
                size: object.size_bytes.unwrap_or(0),
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
    let response = match &payload {
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
            let original = crate::mappings::javascript::apply(&data, file_name, *line, *column)?;
            ApplyResponse::JavaScript { ok: true, original }
        }
        ApplyPayload::Proguard {
            build_id,
            stacktrace,
        } => {
            require_non_empty("build_id", build_id)?;
            require_non_empty("stacktrace", stacktrace)?;
            let prefix = crate::mappings::proguard::proguard_s3_prefix(auth.project_id, build_id);
            let mut keys = state.storage.list_prefix_keys(&prefix).await?;
            keys.sort_unstable();
            if keys.is_empty() {
                return Err(AppError::NotFound);
            }

            let mut mappings = Vec::with_capacity(keys.len());
            for key in keys {
                mappings.push(state.storage.get(&key).await?);
            }

            let retraced = crate::mappings::proguard::retrace_stacktrace(
                mappings.iter().map(Vec::as_slice),
                stacktrace,
            )?;
            ApplyResponse::Proguard {
                ok: true,
                stacktrace: retraced,
            }
        }
    };

    Ok(Json(response))
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

fn normalize_upload_files(
    build_id: &str,
    uploaded_at: &str,
    files: &[UploadFile],
) -> Result<Vec<(String, String)>, AppError> {
    require_non_empty("build_id", build_id)?;
    require_non_empty("uploaded_at", uploaded_at)?;
    if files.is_empty() {
        return Err(AppError::BadRequest("no files provided".into()));
    }

    let mut normalized = Vec::with_capacity(files.len());
    for entry in files {
        require_non_empty("file_name", &entry.file_name)?;
        require_non_empty("content", &entry.content)?;
        normalized.push((entry.file_name.clone(), entry.content.clone()));
    }
    Ok(normalized)
}

fn parse_sourcemap_key(key: &str) -> Option<(String, String)> {
    let mut parts = key.splitn(3, '/');
    let _project_id = parts.next()?;
    let build_id = parts.next()?.to_string();
    let file_name = parts.next()?.to_string();
    Some((build_id, file_name))
}

fn validate_project_sourcemap_key(project_id: Uuid, s3_key: &str) -> Result<String, AppError> {
    require_non_empty("s3_key", s3_key)?;

    let prefix = format!("{project_id}/");
    if !s3_key.starts_with(&prefix) || parse_sourcemap_key(s3_key).is_none() {
        return Err(AppError::BadRequest("invalid sourcemap key".into()));
    }

    Ok(s3_key.to_string())
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
        UploadFile, normalize_upload_files, normalized_build_ids, parse_sourcemap_key,
        select_builds_for_cleanup, validate_project_sourcemap_key,
    };
    use crate::mappings::{javascript::map_file_name, require_non_empty};
    use crate::storage::StoredObjectMeta;
    use uuid::Uuid;

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
    fn parse_sourcemap_key_keeps_nested_proguard_file_name() {
        let parsed = parse_sourcemap_key("proj-id/build-42/proguard/base.txt");
        assert_eq!(
            parsed,
            Some(("build-42".to_string(), "proguard/base.txt".to_string()))
        );
    }

    #[test]
    fn validate_project_sourcemap_key_accepts_owned_key() {
        let project_id = Uuid::parse_str("01954b9b-7b1d-72b8-8af3-f8d058f60b79").unwrap();
        let key = format!("{project_id}/build-42/chunk.js.map");

        let validated = validate_project_sourcemap_key(project_id, &key).unwrap();

        assert_eq!(validated, key);
    }

    #[test]
    fn validate_project_sourcemap_key_rejects_foreign_key() {
        let project_id = Uuid::parse_str("01954b9b-7b1d-72b8-8af3-f8d058f60b79").unwrap();
        let foreign_project_id = Uuid::parse_str("01954b9b-8228-7d29-9d18-c97b9fb3f924").unwrap();
        let key = format!("{foreign_project_id}/build-42/chunk.js.map");

        let err = validate_project_sourcemap_key(project_id, &key).expect_err("key should fail");

        assert!(format!("{err}").contains("invalid sourcemap key"));
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
    fn normalize_upload_files_accepts_multiple_named_files() {
        let normalized = normalize_upload_files(
            "build-1",
            "2026-03-22T00:00:00Z",
            &[
                UploadFile {
                    file_name: "base.txt".to_string(),
                    content: "one".to_string(),
                },
                UploadFile {
                    file_name: "feature.txt".to_string(),
                    content: "two".to_string(),
                },
            ],
        )
        .expect("entries should normalize");

        assert_eq!(
            normalized,
            vec![
                ("base.txt".to_string(), "one".to_string()),
                ("feature.txt".to_string(), "two".to_string()),
            ]
        );
    }

    #[test]
    fn normalize_upload_files_rejects_empty_payload() {
        let err = normalize_upload_files("build-1", "2026-03-22T00:00:00Z", &[])
            .expect_err("empty payload should fail");

        assert!(format!("{err}").contains("no files provided"));
    }

    #[test]
    fn normalize_upload_files_rejects_empty_content() {
        let err = normalize_upload_files(
            "build-1",
            "2026-03-22T00:00:00Z",
            &[UploadFile {
                file_name: "base.txt".to_string(),
                content: "   ".to_string(),
            }],
        )
        .expect_err("blank file content should be rejected");

        assert!(format!("{err}").contains("content is required"));
    }

    #[test]
    fn select_builds_for_cleanup_keeps_latest_and_excluded() {
        let objects = vec![
            StoredObjectMeta {
                key: "proj/build-001/app.js.map".to_string(),
                last_modified_epoch_seconds: Some(100),
                size_bytes: None,
            },
            StoredObjectMeta {
                key: "proj/build-002/app.js.map".to_string(),
                last_modified_epoch_seconds: Some(200),
                size_bytes: None,
            },
            StoredObjectMeta {
                key: "proj/build-003/app.js.map".to_string(),
                last_modified_epoch_seconds: Some(400),
                size_bytes: None,
            },
            StoredObjectMeta {
                key: "proj/build-004/app.js.map".to_string(),
                last_modified_epoch_seconds: Some(300),
                size_bytes: None,
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
                size_bytes: None,
            },
            StoredObjectMeta {
                key: "proj/build-a/b.js.map".to_string(),
                last_modified_epoch_seconds: Some(500),
                size_bytes: None,
            },
            StoredObjectMeta {
                key: "proj/build-b/a.js.map".to_string(),
                last_modified_epoch_seconds: Some(400),
                size_bytes: None,
            },
        ];

        let (latest, deleted) = select_builds_for_cleanup(&objects, &[]);
        assert_eq!(latest, Some("build-a".to_string()));
        assert_eq!(deleted, vec!["build-b".to_string()]);
    }
}
