pub mod archive;
pub mod cdn;
pub mod download;
pub mod resolve;

use anyhow::{Context, Result};
use cascette_crypto::EncodingKey;

pub use cdn::Product;

use crate::archive::ArchiveManager;
use crate::cdn::CdnSession;

/// Download and decompress content addressed by a Blizzard CASC encoding key.
///
/// `product` is the Blizzard product code, for example `wow` or `wowt`.
/// Archive indexes are loaded from `~/.cache/casc-extract/<product>-<build>/indices`
/// when present, or downloaded from Blizzard's CDN and persisted there when missing.
pub async fn fetch_encoding_key(product: &str, encoding_key: &EncodingKey) -> Result<Vec<u8>> {
    let product = Product::new(product)?;
    let session = CdnSession::connect_product(product).await?;
    let archive_manager = load_or_init_archive_manager(&session).await?;

    download::download_file(&archive_manager, &session, encoding_key).await
}

/// Parse an encoding key as hex, then download and decompress it from Blizzard's CDN.
pub async fn fetch_encoding_key_hex(product: &str, encoding_key_hex: &str) -> Result<Vec<u8>> {
    let encoding_key = EncodingKey::from_hex(encoding_key_hex)
        .with_context(|| format!("invalid encoding key hex: {encoding_key_hex}"))?;

    fetch_encoding_key(product, &encoding_key).await
}

/// Synchronous wrapper around [`fetch_encoding_key`].
pub fn fetch_encoding_key_blocking(product: &str, encoding_key: &EncodingKey) -> Result<Vec<u8>> {
    let runtime = tokio::runtime::Runtime::new().context("failed to create Tokio runtime")?;
    runtime.block_on(fetch_encoding_key(product, encoding_key))
}

/// Synchronous wrapper around [`fetch_encoding_key_hex`].
pub fn fetch_encoding_key_hex_blocking(product: &str, encoding_key_hex: &str) -> Result<Vec<u8>> {
    let runtime = tokio::runtime::Runtime::new().context("failed to create Tokio runtime")?;
    runtime.block_on(fetch_encoding_key_hex(product, encoding_key_hex))
}

async fn load_or_init_archive_manager(session: &CdnSession) -> Result<ArchiveManager> {
    match ArchiveManager::load_cached(&session.cache_dir()) {
        Ok(manager) => Ok(manager),
        Err(load_error) => ArchiveManager::init(session).await.with_context(|| {
            format!(
                "failed to load cached archive indices ({load_error:#}) or download fresh indices"
            )
        }),
    }
}
