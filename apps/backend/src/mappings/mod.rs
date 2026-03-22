pub mod javascript;
pub mod proguard;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AppError;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MappingType {
    JavaScript,
    Proguard,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginalPosition {
    pub source: String,
    pub line: u32,
    pub column: u32,
    pub name: Option<String>,
}

pub(crate) fn require_non_empty(field: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() {
        return Err(AppError::BadRequest(format!("{field} is required")));
    }
    Ok(())
}

pub(crate) fn s3_key(project_id: Uuid, build_id: &str, file_name: &str) -> String {
    format!("{project_id}/{build_id}/{file_name}")
}
