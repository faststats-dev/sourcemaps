mod auth;
mod config;
mod crypto;
mod error;
mod routes;
mod storage;

use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;
use tracing::info;

use config::Config;
use crypto::Crypto;
use storage::Storage;

pub struct AppState {
    pub db: sqlx::PgPool,
    pub storage: Storage,
    pub apikey_crypto: Arc<Crypto>,
    pub admin_token: Arc<str>,
}

pub type SharedState = Arc<AppState>;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "backend=debug,tower_http=info".into()),
        )
        .init();

    let config = Config::from_env().expect("missing required environment variables");
    let state = Arc::new(build_state(&config).await);

    let public_app = routes::public_router(state.clone());
    let internal_app = routes::internal_router(state);

    let public_listener = tokio::net::TcpListener::bind(config.listen_addr)
        .await
        .expect("failed to bind public listener");
    let internal_listener = tokio::net::TcpListener::bind(config.internal_listen_addr)
        .await
        .expect("failed to bind internal listener");

    info!("public backend listening on {}", config.listen_addr);
    info!(
        "internal backend listening on {}",
        config.internal_listen_addr
    );

    let (public_result, internal_result) = tokio::join!(
        serve_listener(public_listener, public_app),
        serve_listener(internal_listener, internal_app)
    );
    public_result.unwrap();
    internal_result.unwrap();
}

async fn build_state(config: &Config) -> AppState {
    if config.file_encryption_key == config.apikey_encryption_key {
        panic!("FILE_ENCRYPTION_KEY and APIKEY_ENCRYPTION_KEY must be different");
    }

    let file_crypto =
        Arc::new(Crypto::new(&config.file_encryption_key).expect("invalid FILE_ENCRYPTION_KEY"));
    let apikey_crypto = Arc::new(
        Crypto::new(&config.apikey_encryption_key).expect("invalid APIKEY_ENCRYPTION_KEY"),
    );
    let db = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await
        .expect("failed to connect to database");
    let s3_client = s3_client(config);
    let storage = Storage::new(s3_client, config.s3_bucket.clone(), file_crypto.clone());

    AppState {
        db,
        storage,
        apikey_crypto,
        admin_token: Arc::<str>::from(config.admin_token.clone()),
    }
}

fn s3_client(config: &Config) -> aws_sdk_s3::Client {
    let s3_creds = aws_sdk_s3::config::Credentials::new(
        &config.s3_access_key_id,
        &config.s3_secret_key,
        None,
        None,
        "env",
    );

    aws_sdk_s3::Client::from_conf(
        aws_sdk_s3::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(config.s3_region.clone()))
            .credentials_provider(s3_creds)
            .endpoint_url(&config.s3_endpoint)
            .force_path_style(true)
            .build(),
    )
}

async fn serve_listener(
    listener: tokio::net::TcpListener,
    app: axum::Router<()>,
) -> std::io::Result<()> {
    axum::serve(listener, app).await
}
