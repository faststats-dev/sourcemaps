use std::io::{Read, Write};
use std::sync::Arc;

use aws_sdk_s3::Client;
use aws_sdk_s3::types::{Delete, ObjectIdentifier};

use crate::crypto::Crypto;
use crate::error::AppError;

const ZSTD_LEVEL: i32 = 3;

#[derive(Clone)]
pub struct Storage {
    client: Client,
    bucket: String,
    crypto: Arc<Crypto>,
}

#[derive(Clone, Debug)]
pub struct StoredObjectMeta {
    pub key: String,
    pub last_modified_epoch_seconds: Option<i64>,
    pub size_bytes: Option<u64>,
}

impl Storage {
    pub fn new(client: Client, bucket: String, crypto: Arc<Crypto>) -> Self {
        Self {
            client,
            bucket,
            crypto,
        }
    }

    pub async fn put(&self, key: &str, plaintext: &[u8]) -> Result<(), AppError> {
        let compressed = zstd_compress(plaintext)?;
        let encrypted = self.crypto.encrypt(&compressed)?;

        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(encrypted.into())
            .send()
            .await
            .map_err(s3_error)?;

        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<Vec<u8>, AppError> {
        let resp = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(s3_get_error)?;

        let encrypted = resp.body.collect().await.map_err(s3_error)?.to_vec();

        let compressed = self.crypto.decrypt(&encrypted)?;
        zstd_decompress(&compressed)
    }

    pub async fn delete_prefix(&self, prefix: &str) -> Result<u64, AppError> {
        let mut deleted: u64 = 0;
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);

            if let Some(token) = &continuation_token {
                req = req.continuation_token(token);
            }

            let resp = req.send().await.map_err(s3_error)?;

            let objects: Vec<ObjectIdentifier> = resp
                .contents()
                .iter()
                .filter_map(|obj| ObjectIdentifier::builder().key(obj.key()?).build().ok())
                .collect();

            if !objects.is_empty() {
                let count = objects.len() as u64;
                let delete = Delete::builder()
                    .set_objects(Some(objects))
                    .build()
                    .map_err(s3_error)?;

                self.client
                    .delete_objects()
                    .bucket(&self.bucket)
                    .delete(delete)
                    .send()
                    .await
                    .map_err(s3_error)?;

                deleted += count;
            }

            if resp.is_truncated() != Some(true) {
                break;
            }
            continuation_token = resp.next_continuation_token().map(Into::into);
        }

        Ok(deleted)
    }

    pub async fn delete_keys(&self, keys: &[String]) -> Result<u64, AppError> {
        let mut deleted: u64 = 0;

        for chunk in keys.chunks(1000) {
            let objects: Vec<ObjectIdentifier> = chunk
                .iter()
                .filter_map(|key| ObjectIdentifier::builder().key(key).build().ok())
                .collect();

            if objects.is_empty() {
                continue;
            }

            let count = objects.len() as u64;
            let delete = Delete::builder()
                .set_objects(Some(objects))
                .build()
                .map_err(s3_error)?;

            self.client
                .delete_objects()
                .bucket(&self.bucket)
                .delete(delete)
                .send()
                .await
                .map_err(s3_error)?;

            deleted += count;
        }

        Ok(deleted)
    }

    pub async fn list_prefix_keys(&self, prefix: &str) -> Result<Vec<String>, AppError> {
        let mut keys = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);

            if let Some(token) = &continuation_token {
                req = req.continuation_token(token);
            }

            let resp = req.send().await.map_err(s3_error)?;

            for object in resp.contents() {
                if let Some(key) = object.key() {
                    keys.push(key.to_string());
                }
            }

            if resp.is_truncated() != Some(true) {
                break;
            }
            continuation_token = resp.next_continuation_token().map(Into::into);
        }

        Ok(keys)
    }

    pub async fn list_prefix_objects(
        &self,
        prefix: &str,
    ) -> Result<Vec<StoredObjectMeta>, AppError> {
        let mut objects = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix);

            if let Some(token) = &continuation_token {
                req = req.continuation_token(token);
            }

            let resp = req.send().await.map_err(s3_error)?;

            for object in resp.contents() {
                if let Some(key) = object.key() {
                    objects.push(StoredObjectMeta {
                        key: key.to_string(),
                        last_modified_epoch_seconds: object.last_modified().map(|dt| dt.secs()),
                        size_bytes: object.size().and_then(|size| u64::try_from(size).ok()),
                    });
                }
            }

            if resp.is_truncated() != Some(true) {
                break;
            }
            continuation_token = resp.next_continuation_token().map(Into::into);
        }

        Ok(objects)
    }
}

fn zstd_compress(data: &[u8]) -> Result<Vec<u8>, AppError> {
    let mut encoder = zstd::Encoder::new(Vec::new(), ZSTD_LEVEL)
        .map_err(|e| AppError::Compression(e.to_string()))?;
    encoder
        .write_all(data)
        .map_err(|e| AppError::Compression(e.to_string()))?;
    encoder
        .finish()
        .map_err(|e| AppError::Compression(e.to_string()))
}

fn zstd_decompress(data: &[u8]) -> Result<Vec<u8>, AppError> {
    let mut decoder = zstd::Decoder::new(data).map_err(|e| AppError::Compression(e.to_string()))?;
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| AppError::Compression(e.to_string()))?;
    Ok(out)
}

fn s3_error(error: impl ToString + std::fmt::Debug) -> AppError {
    let display = error.to_string();
    let debug = format!("{error:?}");
    if display == debug {
        AppError::S3(display)
    } else {
        AppError::S3(format!("{display}; details: {debug}"))
    }
}

fn s3_get_error(error: impl ToString + std::fmt::Debug) -> AppError {
    let msg = error.to_string();
    let details = format!("{error:?}");
    if msg.contains("NoSuchKey")
        || msg.contains("not found")
        || details.contains("NoSuchKey")
        || details.contains("NotFound")
    {
        AppError::NotFound
    } else {
        AppError::S3(format!("{msg}; details: {details}"))
    }
}
