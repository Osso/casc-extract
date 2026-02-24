use std::path::PathBuf;

use anyhow::{Context, Result};
use cascette_client_storage::resolver::ContentResolver;
use cascette_crypto::ContentKey;
use cascette_formats::CascFormat;
use cascette_formats::blte::BlteFile;
use cascette_formats::config::BuildConfig;
use cascette_formats::config::CdnConfig as CascCdnConfig;
use cascette_protocol::cdn::CdnEndpoint;
use cascette_protocol::{CdnClient, CdnConfig, ClientConfig, ContentType, RibbitTactClient};

pub struct CdnSession {
    cdn_client: CdnClient,
    endpoint: CdnEndpoint,
    build_config_key: Vec<u8>,
    cdn_config_key: Vec<u8>,
    build_id: String,
}

impl CdnSession {
    pub async fn connect() -> Result<Self> {
        let client = RibbitTactClient::new(ClientConfig::default())
            .context("failed to create RibbitTactClient")?;

        let cdns = client
            .query("v1/products/wow/cdns")
            .await
            .context("failed to query CDNs")?;

        let versions = client
            .query("v1/products/wow/versions")
            .await
            .context("failed to query versions")?;

        let endpoint = extract_endpoint(&cdns)?;
        let (build_config_key, cdn_config_key, build_id) = extract_version_fields(&versions)?;

        let cdn_client = CdnClient::new(client.cache().clone(), CdnConfig::default())
            .context("failed to create CdnClient")?;

        Ok(Self {
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
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home)
            .join(".cache")
            .join("casc-extract")
            .join(format!("wow-{}", self.build_id))
    }

    pub fn build_id(&self) -> &str {
        &self.build_id
    }

    pub fn cdn_client(&self) -> &CdnClient {
        &self.cdn_client
    }

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
        let scheme = self
            .endpoint
            .scheme
            .as_deref()
            .unwrap_or("https");
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
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let cache_dir = PathBuf::from(home)
        .join(".cache")
        .join("casc-extract")
        .join(format!("wow-{build_id}"));

    let root = std::fs::read(cache_dir.join("root.bin"))
        .with_context(|| format!("failed to read root.bin from {}", cache_dir.display()))?;
    let encoding = std::fs::read(cache_dir.join("encoding.bin"))
        .with_context(|| format!("failed to read encoding.bin from {}", cache_dir.display()))?;

    Ok((root, encoding))
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
    let cdn_config_key =
        hex::decode(cdn_config_hex).context("invalid hex in CDNConfig field")?;

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
