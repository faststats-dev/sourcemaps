# Sourcemaps

> [!IMPORTANT]
> This repository is in early development and not intended for production use.

A monorepo for sourcemap upload infrastructure.

## Structure

- **`apps/backend`** — Rust (Axum) API server that ingests sourcemap uploads
- **`packages/bundler-plugin`** — Universal unplugin adapter set (Vite, Rollup, Rolldown, Webpack, Rspack, esbuild, Unloader, Farm, Bun) that uploads sourcemaps after builds

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
