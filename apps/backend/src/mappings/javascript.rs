use crate::error::AppError;

use super::OriginalPosition;

pub fn apply(data: &[u8], line: u32, column: u32) -> Result<OriginalPosition, AppError> {
    let source_map = sourcemap::SourceMap::from_slice(data)
        .map_err(|e| AppError::BadRequest(format!("invalid sourcemap: {e}")))?;
    let token = source_map
        .lookup_token(line.saturating_sub(1), column.saturating_sub(1))
        .ok_or(AppError::NotFound)?;
    let source = token.get_source().ok_or(AppError::NotFound)?;
    let src_line = token.get_src_line();
    let src_col = token.get_src_col();

    if src_line == u32::MAX || src_col == u32::MAX {
        return Err(AppError::NotFound);
    }

    Ok(OriginalPosition {
        source: source.to_string(),
        line: src_line.saturating_add(1),
        column: src_col.saturating_add(1),
        name: token.get_name().map(ToString::to_string),
    })
}

pub fn map_file_name(file_name: &str) -> String {
    if file_name.ends_with(".map") {
        file_name.to_string()
    } else {
        format!("{file_name}.map")
    }
}
