package dev.faststats.proguard

data class SourcemapUpload(
    val fileName: String,
    val mapping: String,
)

data class SourcemapUploadPayload(
    val buildId: String,
    val mappingType: String,
    val uploadedAt: String,
    val mappings: List<SourcemapUpload>,
)
