use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cascette_client_storage::resolver::ContentResolver;
use cascette_crypto::ContentKey;
use cascette_formats::CascFormat;
use cascette_formats::blte::BlteFile;
use cascette_formats::config::BuildConfig;
use cascette_formats::config::CdnConfig as CascCdnConfig;
use cascette_protocol::cdn::CdnEndpoint;
use cascette_protocol::{CdnClient, CdnConfig, ClientConfig, ContentType, RibbitTactClient};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    name: String,
}

impl Product {
    pub fn new(name: impl Into<String>) -> Result<Self> {
        let name = name.into();
        validate_product_name(&name)?;
        Ok(Self { name })
    }

    pub fn wow() -> Self {
        Self {
            name: "wow".to_string(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn cdns_query_path(&self) -> String {
        format!("v1/products/{}/cdns", self.name)
    }

    pub fn versions_query_path(&self) -> String {
        format!("v1/products/{}/versions", self.name)
    }

    pub fn build_cache_dir(&self, cache_root: &Path, build_id: &str) -> PathBuf {
        cache_root.join(format!("{}-{build_id}", self.name))
    }
}

impl Default for Product {
    fn default() -> Self {
        Self::wow()
    }
}

pub struct CdnSession {
    product: Product,
    cdn_client: CdnClient,
    endpoint: CdnEndpoint,
    build_config_key: Vec<u8>,
    cdn_config_key: Vec<u8>,
    build_id: String,
}

impl CdnSession {
    pub async fn connect() -> Result<Self> {
        Self::connect_product(Product::wow()).await
    }

    pub async fn connect_product(product: Product) -> Result<Self> {
        let client = RibbitTactClient::new(ClientConfig::default())
            .context("failed to create RibbitTactClient")?;

        let cdns = client
            .query(&product.cdns_query_path())
            .await
            .with_context(|| format!("failed to query CDNs for product {}", product.name()))?;

        let versions = client
            .query(&product.versions_query_path())
            .await
            .with_context(|| format!("failed to query versions for product {}", product.name()))?;

        let endpoint = extract_endpoint(&cdns)?;
        let (build_config_key, cdn_config_key, build_id) = extract_version_fields(&versions)?;

        let cdn_client = CdnClient::new(client.cache().clone(), CdnConfig::default())
            .context("failed to create CdnClient")?;

        Ok(Self {
            product,
            cdn_client,
            endpoint,
            build_config_key,
            cdn_config_key,
            build_id,
        })
    }

    pub async fn init(&self) -> Result<PathBuf> {
        let cache_dir = self.cache_dir();
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("failed to create cache dir: {}", cache_dir.display()))?;

        let build_config = self.download_build_config().await?;

        // Encoding has its own encoding_key in BuildConfig — download first
        let enc_ekey = encoding_cdn_key(&build_config)?;
        eprintln!("  Downloading encoding...");
        let encoding_data = self
            .download_and_decompress(ContentType::Data, &enc_ekey)
            .await
            .context("failed to download encoding")?;

        // Root only has a content key — look it up in encoding to get CDN key
        let root_ekey = root_cdn_key(&build_config, &encoding_data)?;
        eprintln!("  Downloading root...");
        let root_data = self
            .download_and_decompress(ContentType::Data, &root_ekey)
            .await
            .context("failed to download root")?;

        write_cache_file(&cache_dir.join("root.bin"), &root_data)?;
        write_cache_file(&cache_dir.join("encoding.bin"), &encoding_data)?;

        Ok(cache_dir)
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.product
            .build_cache_dir(&default_cache_root(), &self.build_id)
    }

    pub fn build_id(&self) -> &str {
        &self.build_id
    }

    pub fn product(&self) -> &Product {
        &self.product
    }

    #[allow(dead_code)]
    pub fn cdn_client(&self) -> &CdnClient {
        &self.cdn_client
    }

    #[allow(dead_code)]
    pub fn endpoint(&self) -> &CdnEndpoint {
        &self.endpoint
    }

    pub async fn download_cdn_config(&self) -> Result<CascCdnConfig> {
        let raw = self
            .cdn_client
            .download(&self.endpoint, ContentType::Config, &self.cdn_config_key)
            .await
            .context("failed to download CDN config")?;
        CascCdnConfig::parse(raw.as_slice())
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("failed to parse CDN config")
    }

    pub fn build_data_url(&self, hash: &str) -> String {
        let scheme = self.endpoint.scheme.as_deref().unwrap_or("https");
        format!(
            "{}://{}/{}/data/{}/{}/{}",
            scheme,
            self.endpoint.host,
            self.endpoint.path,
            &hash[..2],
            &hash[2..4],
            hash
        )
    }

    async fn download_build_config(&self) -> Result<BuildConfig> {
        let raw = self
            .cdn_client
            .download(&self.endpoint, ContentType::Config, &self.build_config_key)
            .await
            .context("failed to download BuildConfig")?;
        BuildConfig::parse(raw.as_slice())
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("failed to parse BuildConfig")
    }

    async fn download_and_decompress(
        &self,
        content_type: ContentType,
        key: &[u8],
    ) -> Result<Vec<u8>> {
        let raw = self
            .cdn_client
            .download(&self.endpoint, content_type, key)
            .await
            .context("failed to download content")?;
        let blte = BlteFile::parse(&raw)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("failed to parse BLTE")?;
        blte.decompress()
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("failed to decompress BLTE")
    }
}

pub fn load_cached(build_id: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    load_cached_product(&Product::wow(), build_id)
}

pub fn load_cached_product(product: &Product, build_id: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let cache_dir = product.build_cache_dir(&default_cache_root(), build_id);

    let root = std::fs::read(cache_dir.join("root.bin"))
        .with_context(|| format!("failed to read root.bin from {}", cache_dir.display()))?;
    let encoding = std::fs::read(cache_dir.join("encoding.bin"))
        .with_context(|| format!("failed to read encoding.bin from {}", cache_dir.display()))?;

    Ok((root, encoding))
}

pub fn default_cache_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".cache").join("casc-extract")
}

fn validate_product_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if valid {
        Ok(())
    } else {
        anyhow::bail!("invalid product name: {name}")
    }
}

fn extract_endpoint(cdns: &cascette_formats::bpsv::BpsvDocument) -> Result<CdnEndpoint> {
    let row = cdns.rows().first().context("CDNs response has no rows")?;
    CdnClient::endpoint_from_bpsv_row(row, cdns.schema())
        .context("failed to extract CDN endpoint from BPSV row")
}

fn extract_version_fields(
    versions: &cascette_formats::bpsv::BpsvDocument,
) -> Result<(Vec<u8>, Vec<u8>, String)> {
    let row = versions
        .rows()
        .first()
        .context("versions response has no rows")?;
    let schema = versions.schema();

    let build_config_hex = row
        .get_by_name("BuildConfig", schema)
        .map(|v| v.to_string())
        .context("missing BuildConfig field in versions")?;

    let cdn_config_hex = row
        .get_by_name("CDNConfig", schema)
        .map(|v| v.to_string())
        .context("missing CDNConfig field in versions")?;

    let build_id = row
        .get_by_name("BuildId", schema)
        .map(|v| v.to_string())
        .context("missing BuildId field in versions")?;

    let build_config_key =
        hex::decode(build_config_hex).context("invalid hex in BuildConfig field")?;
    let cdn_config_key = hex::decode(cdn_config_hex).context("invalid hex in CDNConfig field")?;

    Ok((build_config_key, cdn_config_key, build_id))
}

fn encoding_cdn_key(config: &BuildConfig) -> Result<Vec<u8>> {
    let ekey_hex = config
        .encoding_key()
        .context("missing encoding key in BuildConfig")?;
    hex::decode(ekey_hex).context("invalid hex in encoding key")
}

fn root_cdn_key(config: &BuildConfig, encoding_data: &[u8]) -> Result<Vec<u8>> {
    let root_hex = config.root().context("missing root in BuildConfig")?;
    let root_ckey =
        ContentKey::from_hex(root_hex).map_err(|e| anyhow::anyhow!("invalid root hex: {e}"))?;

    let resolver = ContentResolver::new();
    resolver
        .load_encoding_file(encoding_data)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("failed to parse encoding file")?;

    let ekey = resolver
        .resolve_content_key(&root_ckey)
        .context("root content key not found in encoding file")?;

    Ok(ekey.as_bytes().to_vec())
}

fn write_cache_file(path: &PathBuf, data: &[u8]) -> Result<()> {
    std::fs::write(path, data)
        .with_context(|| format!("failed to write cache file: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn product_query_paths_include_requested_product() {
        let product = Product::new("wowt").expect("valid product");

        assert_eq!(product.cdns_query_path(), "v1/products/wowt/cdns");
        assert_eq!(product.versions_query_path(), "v1/products/wowt/versions");
    }

    #[test]
    fn product_rejects_path_injection() {
        let err = Product::new("../wow").expect_err("invalid product must fail");

        assert!(err.to_string().contains("invalid product"));
    }

    #[test]
    fn product_build_cache_dir_uses_cache_root_product_and_build_id() {
        let product = Product::new("wowt").expect("valid product");
        let dir = product.build_cache_dir(Path::new("/tmp/cache/casc-extract"), "12345");

        assert_eq!(dir, Path::new("/tmp/cache/casc-extract/wowt-12345"));
    }
}
