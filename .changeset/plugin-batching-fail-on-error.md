---
"@faststats/sourcemap-uploader-plugin": patch
---

Add upload payload batching with a configurable `maxUploadBodyBytes` limit and introduce a `failOnError` option to control whether upload failures should fail the build.
