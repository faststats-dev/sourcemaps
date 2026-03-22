package dev.faststats.proguard

data class SourcemapUpload(
    val fileName: String,
    val sourcemap: String,
)

data class SourcemapUploadPayload(
    val buildId: String,
    val mappingType: String,
    val mapping: String,
    val uploadedAt: String,
    val sourcemaps: List<SourcemapUpload>,
)
