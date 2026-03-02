---
"@sourcemaps/backend": patch
---

Increase the public sourcemap ingest request body limit to 50MB to avoid 413 errors when uploading larger sourcemap payloads.
