# @faststats/sourcemap-uploader-plugin

## 0.2.1

### Patch Changes

- bd0e180: Increase the public sourcemap ingest request body limit to 50MB to avoid 413 errors when uploading larger sourcemap payloads.

## 0.2.1

### Patch Changes

- bd0e180: Add upload payload batching with a configurable `maxUploadBodyBytes` limit and introduce a `failOnError` option to control whether upload failures should fail the build.

## 0.2.0

### Minor Changes

- d011a2d: feat: add option to enable/disable sending

### Patch Changes

- af723ad: rename sourcemapsPlugin to unpluginInstance and export default correct

## 0.1.1

### Patch Changes

- 5ab8013: fix: add proper file outputs
