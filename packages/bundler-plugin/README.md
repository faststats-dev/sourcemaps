# @faststats/sourcemap-uploader-plugin

> [!IMPORTANT]
> This repository is in early development and not intended for production use.
> Its only published for internal testing.

Unplugin-based sourcemap uploader that works across all unplugin adapters:

- Vite
- Rollup
- Rolldown
- Webpack
- Rspack
- esbuild
- Unloader
- Farm
- Bun

## What it does

- injects build metadata in the bundle at `globalThis.__SOURCEMAPS_BUILD__`
- uploads generated sourcemaps to a backend endpoint
- supports custom `buildId` or auto-generated IDs
- prefers native bundler build IDs when available (webpack/rspack compilation hash)
- optionally deletes sourcemap files after successful upload

## Usage

```ts
import sourcemapsPlugin from "@faststats/sourcemap-uploader-plugin/vite";

export default {
  plugins: [
    sourcemapsPlugin({
      endpoint: "http://localhost:3000/v0/upload",
      deleteAfterUpload: true,
    }),
  ],
};
```

## Options

- `endpoint` (required): upload URL
- `enabled`: `true` by default; set `false` or pass `(framework) => boolean` to disable the plugin conditionally
- `authToken`: bearer token for the upload request
- `buildId`: custom build identifier
- `deleteAfterUpload`: remove sourcemap files after successful upload
- `globalKey`: runtime global key for build metadata
- `fetchImpl`: custom fetch implementation
- `onUploadSuccess`: callback after successful upload
- `onUploadError`: callback when upload fails

## Upload payload

The plugin sends `POST /v0/upload` with this JSON body:

```json
{
  "type": "javascript",
  "buildId": "build-123",
  "uploadedAt": "2026-04-01T12:00:00.000Z",
  "files": [
    {
      "fileName": "assets/app.js.map",
      "content": "{...}"
    }
  ]
}
```

## Tests

```bash
bun test
```
