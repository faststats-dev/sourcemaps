FROM rust:1.98.1-trixie AS chef
RUN rustup target add x86_64-unknown-linux-musl
RUN cargo install cargo-chef --version 0.1.78 --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools pkg-config \
    && rm -rf /var/lib/apt/lists/*
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook \
    --locked \
    --release \
    --target x86_64-unknown-linux-musl \
    --recipe-path recipe.json
COPY . .
RUN cargo build \
    --locked \
    --release \
    --target x86_64-unknown-linux-musl \
    --bin sourcemaps

FROM gcr.io/distroless/static-debian13:nonroot AS runtime
WORKDIR /app
COPY --from=builder \
    /app/target/x86_64-unknown-linux-musl/release/sourcemaps \
    /usr/local/bin/sourcemaps
USER nonroot:nonroot

ENV PORT=8080
EXPOSE $PORT

ENTRYPOINT ["/usr/local/bin/sourcemaps"]
