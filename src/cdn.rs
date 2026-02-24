use std::path::PathBuf;

use anyhow::{Context, Result};
use cascette_formats::CascFormat;
use cascette_formats::blte::BlteFile;
use cascette_formats::config::BuildConfig;
use cascette_protocol::cdn::CdnEndpoint;
use cascette_protocol::{CdnClient, CdnConfig, ClientConfig, ContentType, RibbitTactClient};

pub struct CdnSession {
    cdn_client: CdnClient,
    endpoint: CdnEndpoint,
    build_config_key: Vec<u8>,
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
        let (build_config_key, build_id) = extract_version_fields(&versions)?;

        let cdn_client = CdnClient::new(client.cache().clone(), CdnConfig::default())
            .context("failed to create CdnClient")?;

        Ok(Self {
            cdn_client,
            endpoint,
            build_config_key,
            build_id,
        })
    }

    pub async fn init(&self) -> Result<PathBuf> {
        let cache_dir = self.cache_dir();
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("failed to create cache dir: {}", cache_dir.display()))?;

        let build_config = self.download_build_config().await?;
        let (root_key, encoding_key) = parse_root_and_encoding_keys(&build_config)?;

        let root_data = self
            .download_and_decompress(ContentType::Data, &root_key)
            .await
            .context("failed to download root")?;
        let encoding_data = self
            .download_and_decompress(ContentType::Data, &encoding_key)
            .await
            .context("failed to download encoding")?;

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

    pub fn cdn_client(&self) -> &CdnClient {
        &self.cdn_client
    }

    pub fn endpoint(&self) -> &CdnEndpoint {
        &self.endpoint
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
) -> Result<(Vec<u8>, String)> {
    let row = versions
        .rows()
        .first()
        .context("versions response has no rows")?;
    let schema = versions.schema();

    let build_config_hex = row
        .get_by_name("BuildConfig", schema)
        .and_then(|v| v.as_string())
        .context("missing BuildConfig field in versions")?;

    let build_id = row
        .get_by_name("BuildId", schema)
        .and_then(|v| v.as_string())
        .context("missing BuildId field in versions")?
        .to_string();

    let build_config_key =
        hex::decode(build_config_hex).context("invalid hex in BuildConfig field")?;

    Ok((build_config_key, build_id))
}

fn parse_root_and_encoding_keys(config: &BuildConfig) -> Result<(Vec<u8>, Vec<u8>)> {
    let root_hex = config.root().context("missing root field in BuildConfig")?;
    let root_key = hex::decode(root_hex).context("invalid hex in root field")?;

    let encoding_info = config
        .encoding()
        .context("missing encoding field in BuildConfig")?;
    let encoding_key =
        hex::decode(&encoding_info.content_key).context("invalid hex in encoding field")?;

    Ok((root_key, encoding_key))
}

fn write_cache_file(path: &PathBuf, data: &[u8]) -> Result<()> {
    std::fs::write(path, data)
        .with_context(|| format!("failed to write cache file: {}", path.display()))
}
