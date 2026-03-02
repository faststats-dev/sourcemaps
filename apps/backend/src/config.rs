use std::net::SocketAddr;

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub file_encryption_key: String,
    pub apikey_encryption_key: String,
    pub s3_bucket: String,
    pub s3_region: String,
    pub s3_endpoint: String,
    pub s3_access_key_id: String,
    pub s3_secret_access_key: String,
    pub listen_addr: SocketAddr,
    pub internal_listen_addr: SocketAddr,
    pub admin_token: String,
}

impl Config {
    pub fn from_env() -> Result<Self, std::env::VarError> {
        let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into());
        let port: u16 = std::env::var("PORT")
            .unwrap_or_else(|_| "3000".into())
            .parse()
            .expect("PORT must be a valid u16");
        let internal_host = std::env::var("INTERNAL_HOST").unwrap_or_else(|_| "127.0.0.1".into());
        let internal_port: u16 = std::env::var("INTERNAL_PORT")
            .unwrap_or_else(|_| "3001".into())
            .parse()
            .expect("INTERNAL_PORT must be a valid u16");

        Ok(Self {
            database_url: std::env::var("DATABASE_URL")?,
            file_encryption_key: std::env::var("FILE_ENCRYPTION_KEY")
                .or_else(|_| std::env::var("ENCRYPTION_KEY"))?,
            apikey_encryption_key: std::env::var("APIKEY_ENCRYPTION_KEY")
                .or_else(|_| std::env::var("SOURCEMAP_API_KEY_SECRET"))?,
            s3_bucket: std::env::var("S3_BUCKET")?,
            s3_region: std::env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".into()),
            s3_endpoint: std::env::var("S3_ENDPOINT")?,
            s3_access_key_id: std::env::var("S3_ACCESS_KEY_ID")?,
            s3_secret_access_key: std::env::var("S3_SECRET_ACCESS_KEY")?,
            listen_addr: SocketAddr::new(host.parse().expect("HOST must be a valid IP"), port),
            internal_listen_addr: SocketAddr::new(
                internal_host
                    .parse()
                    .expect("INTERNAL_HOST must be a valid IP"),
                internal_port,
            ),
            admin_token: std::env::var("ADMIN_TOKEN")?,
        })
    }
}
