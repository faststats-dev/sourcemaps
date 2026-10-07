# Sourcemaps
The old monorepo for sourcemap upload infrastructure.

> [!IMPORTANT]
> The ProGuard upload moved to https://github.com/faststats-dev/proguard-upload-plugin

> [!IMPORTANT]
> The JavaScript upload moved into https://github.com/faststats-dev/faststats-javascript

> [!IMPORTANT]
> The Rust service moved into https://github.com/faststats-dev/data-collector

## Structure

- **`apps/backend`** — Rust (Axum) API server that ingests sourcemap uploads
- **`packages/bundler-plugin`** — Universal unplugin adapter set (Vite, Rolldown, Webpack and more) that uploads sourcemaps after builds
- **`packages/proguard-plugin`** — a Gradle plugin for uploading ProGuard obfuscation mappings

## Development

```sh
bun install        # install JS dependencies
bun run dev        # start all packages in dev mode
bun run build      # build all packages
bun run check-types # type-check all packages
```

### Backend

```sh
cd apps/backend
cargo run          # start the API server on :3000
```

### Bundler plugin tests

```sh
cd packages/bundler-plugin
bun run test
```
