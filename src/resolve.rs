use anyhow::{Context, Result};
use cascette_client_storage::resolver::ContentResolver;
use cascette_crypto::EncodingKey;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub struct Resolver {
    inner: ContentResolver,
    listfile: Vec<(u32, String)>,
}

impl Resolver {
    pub fn from_cached(root_data: &[u8], encoding_data: &[u8]) -> Result<Self> {
        let inner = ContentResolver::new();
        inner
            .load_root_file(root_data)
            .context("Failed to load root file")?;
        inner
            .load_encoding_file(encoding_data)
            .context("Failed to load encoding file")?;
        Ok(Self {
            inner,
            listfile: Vec::new(),
        })
    }

    pub fn load_listfile(&mut self, path: &Path) -> Result<()> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open listfile: {}", path.display()))?;
        let reader = BufReader::new(file);
        self.listfile = parse_listfile_lines(reader.lines());
        Ok(())
    }

    pub fn search(&self, pattern: &str) -> Vec<(u32, &str)> {
        let lower = pattern.to_lowercase();
        self.listfile
            .iter()
            .filter(|(_, p)| p.to_lowercase().contains(&lower))
            .take(100)
            .map(|(id, p)| (*id, p.as_str()))
            .collect()
    }

    pub fn resolve_path(&self, path: &str) -> Result<EncodingKey> {
        let ckey = self
            .inner
            .resolve_path(path)
            .with_context(|| format!("Path not found in root file: {path}"))?;
        self.inner
            .resolve_content_key(&ckey)
            .with_context(|| format!("Content key not found in encoding file for path: {path}"))
    }

    pub fn resolve_fdid(&self, fdid: u32) -> Result<EncodingKey> {
        let ckey = self
            .inner
            .resolve_file_data_id(fdid)
            .with_context(|| format!("FileDataID {fdid} not found in root file"))?;
        self.inner
            .resolve_content_key(&ckey)
            .with_context(|| format!("Content key not found in encoding file for FDID {fdid}"))
    }

    pub fn path_for_fdid(&self, fdid: u32) -> Option<&str> {
        self.listfile
            .iter()
            .find(|(id, _)| *id == fdid)
            .map(|(_, p)| p.as_str())
    }
}

fn parse_listfile_lines<I>(lines: I) -> Vec<(u32, String)>
where
    I: Iterator<Item = std::io::Result<String>>,
{
    lines
        .filter_map(|line| line.ok())
        .filter_map(parse_listfile_line)
        .collect()
}

fn parse_listfile_line(line: String) -> Option<(u32, String)> {
    let (id_str, path) = line.split_once(';')?;
    let id: u32 = id_str.trim().parse().ok()?;
    let path = path.trim().to_string();
    if path.is_empty() {
        return None;
    }
    Some((id, path))
}

pub fn listfile_cache_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".cache")
        .join("casc-extract")
        .join("listfile.csv")
}

pub async fn download_listfile() -> Result<PathBuf> {
    let dest = listfile_cache_path();
    ensure_cache_dir(&dest)?;
    run_curl_download(&dest).await?;
    Ok(dest)
}

fn ensure_cache_dir(dest: &Path) -> Result<()> {
    let dir = dest.parent().expect("cache path has parent");
    std::fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create cache directory: {}", dir.display()))
}

async fn run_curl_download(dest: &Path) -> Result<()> {
    const URL: &str =
        "https://github.com/wowdev/wow-listfile/releases/latest/download/community-listfile.csv";

    let status = tokio::process::Command::new("curl")
        .args(["-fsSL", "-o", dest.to_str().unwrap(), URL])
        .status()
        .await
        .context("Failed to spawn curl")?;

    if !status.success() {
        anyhow::bail!(
            "curl failed with exit code {} while downloading listfile",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}
