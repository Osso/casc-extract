use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cascette_formats::CascFormat;
use cascette_formats::archive::ArchiveIndex;
use cascette_formats::blte::BlteFile;

use crate::cdn::CdnSession;

pub struct ArchiveManager {
    indices: Vec<(String, ArchiveIndex)>,
    http: reqwest::Client,
}

pub struct ArchiveLocation {
    pub archive_hash: String,
    pub offset: u64,
    pub size: u32,
}

impl ArchiveManager {
    pub async fn init(cdn: &CdnSession) -> Result<Self> {
        let cdn_config = cdn
            .download_cdn_config()
            .await
            .context("failed to download CDN config")?;

        let archives = cdn_config.archives();
        eprintln!("Found {} archives in CDN config", archives.len());

        let cache_dir = indices_cache_dir(cdn);
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("failed to create indices dir: {}", cache_dir.display()))?;

        let http = build_http_client()?;
        let mut indices = Vec::with_capacity(archives.len());

        for (i, info) in archives.iter().enumerate() {
            let hash = &info.content_key;
            eprintln!(
                "  [{}/{}] Downloading index {}",
                i + 1,
                archives.len(),
                hash
            );

            let raw = load_or_download_index_file(&http, cdn, &cache_dir, hash)
                .await
                .with_context(|| format!("failed to load or download index for {hash}"))?;
            let index = parse_index(&raw, hash)?;
            indices.push((hash.clone(), index));
        }

        Ok(Self { indices, http })
    }

    pub fn load_cached(cache_dir: &Path) -> Result<Self> {
        let indices_dir = cache_dir.join("indices");
        let http = build_http_client()?;

        let entries = std::fs::read_dir(&indices_dir)
            .with_context(|| format!("failed to read indices dir: {}", indices_dir.display()))?;

        let mut indices = Vec::new();
        for entry in entries {
            let entry = entry.context("failed to read dir entry")?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("index") {
                continue;
            }
            let hash = path
                .file_stem()
                .and_then(|s| s.to_str())
                .context("invalid index filename")?
                .to_string();
            let raw = std::fs::read(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let index = parse_index(&raw, &hash)?;
            indices.push((hash, index));
        }

        if indices.is_empty() {
            anyhow::bail!("no cached archive indices in {}", indices_dir.display());
        }

        eprintln!("Loaded {} cached archive indices", indices.len());
        Ok(Self { indices, http })
    }

    pub fn find(&self, encoding_key: &[u8]) -> Option<ArchiveLocation> {
        for (hash, index) in &self.indices {
            let ekey_len = index.footer.ekey_length as usize;
            let truncated = &encoding_key[..ekey_len.min(encoding_key.len())];
            if let Some(entry) = index.find_entry(truncated) {
                return Some(ArchiveLocation {
                    archive_hash: hash.clone(),
                    offset: entry.offset,
                    size: entry.size,
                });
            }
        }
        None
    }

    pub async fn download(&self, cdn: &CdnSession, encoding_key: &[u8]) -> Result<Vec<u8>> {
        let key_hex = hex::encode(encoding_key);

        if let Some(loc) = self.find(encoding_key) {
            let raw = self.range_download(cdn, &loc).await?;
            return decompress_blte(&raw);
        }

        eprintln!("  Not found in archives, trying loose file for ekey {key_hex}");
        let url = cdn.build_data_url(&key_hex);
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("HTTP GET failed for loose file: {url}"))?;

        let status = resp.status();
        if status.is_success() {
            let raw = resp
                .bytes()
                .await
                .context("failed to read loose file response body")?
                .to_vec();
            return decompress_blte(&raw);
        }

        anyhow::bail!(
            "encoding key not found in any archive index and loose file returned HTTP {}: {}",
            status,
            key_hex
        )
    }

    async fn range_download(&self, cdn: &CdnSession, loc: &ArchiveLocation) -> Result<Vec<u8>> {
        let url = cdn.build_data_url(&loc.archive_hash);
        let end = loc.offset + loc.size as u64 - 1;
        let range_header = format!("bytes={}-{}", loc.offset, end);

        let resp = self
            .http
            .get(&url)
            .header("Range", &range_header)
            .send()
            .await
            .with_context(|| format!("HTTP range request failed for {url}"))?;

        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("HTTP {} for archive range request: {}", status, url);
        }

        resp.bytes()
            .await
            .context("failed to read archive range response body")
            .map(|b| b.to_vec())
    }
}

fn build_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .context("failed to build HTTP client")
}

fn indices_cache_dir(cdn: &CdnSession) -> PathBuf {
    cdn.cache_dir().join("indices")
}

async fn load_or_download_index_file(
    http: &reqwest::Client,
    cdn: &CdnSession,
    cache_dir: &Path,
    hash: &str,
) -> Result<Vec<u8>> {
    match read_cached_index_file(cache_dir, hash) {
        Ok(raw) => {
            eprintln!("  Using cached index {hash}");
            Ok(raw)
        }
        Err(_) => {
            let raw = download_index_file(http, cdn, hash)
                .await
                .with_context(|| format!("failed to download index for {hash}"))?;
            cache_index_file(cache_dir, hash, &raw)?;
            Ok(raw)
        }
    }
}

async fn download_index_file(
    http: &reqwest::Client,
    cdn: &CdnSession,
    hash: &str,
) -> Result<Vec<u8>> {
    let url = format!("{}.index", cdn.build_data_url(hash));
    let resp = http
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET failed: {url}"))?;

    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("HTTP {} fetching index: {}", status, url);
    }

    resp.bytes()
        .await
        .context("failed to read index response body")
        .map(|b| b.to_vec())
}

fn read_cached_index_file(dir: &Path, hash: &str) -> Result<Vec<u8>> {
    let path = dir.join(format!("{hash}.index"));
    std::fs::read(&path).with_context(|| format!("failed to read cached index: {}", path.display()))
}

fn cache_index_file(dir: &Path, hash: &str, data: &[u8]) -> Result<()> {
    let path = dir.join(format!("{hash}.index"));
    std::fs::write(&path, data)
        .with_context(|| format!("failed to write cached index: {}", path.display()))
}

fn parse_index(raw: &[u8], hash: &str) -> Result<ArchiveIndex> {
    let cursor = Cursor::new(raw);
    ArchiveIndex::parse(cursor)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("failed to parse archive index {hash}"))
}

fn decompress_blte(data: &[u8]) -> Result<Vec<u8>> {
    let blte = BlteFile::parse(data)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("failed to parse BLTE from archive data")?;
    blte.decompress()
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("failed to decompress BLTE from archive data")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn load_cached_rejects_empty_index_cache() {
        let cache_dir = unique_temp_dir();
        std::fs::create_dir_all(cache_dir.join("indices")).expect("create indices dir");

        let err = match ArchiveManager::load_cached(&cache_dir) {
            Ok(_) => panic!("empty cache must fail"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("no cached archive indices"));
        std::fs::remove_dir_all(cache_dir).expect("cleanup temp cache dir");
    }

    #[test]
    fn read_cached_index_file_returns_persisted_index_bytes() {
        let cache_dir = unique_temp_dir();
        std::fs::create_dir_all(&cache_dir).expect("create cache dir");
        let hash = "abcdef0123456789";
        let expected = b"cached index bytes";
        std::fs::write(cache_dir.join(format!("{hash}.index")), expected).expect("write index");

        let actual = read_cached_index_file(&cache_dir, hash).expect("read cached index");

        assert_eq!(actual, expected);
        std::fs::remove_dir_all(cache_dir).expect("cleanup temp cache dir");
    }

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("casc-extract-test-{nanos}"))
    }
}
