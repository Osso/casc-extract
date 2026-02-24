use std::path::Path;

use anyhow::{Context, Result};
use cascette_crypto::EncodingKey;
use cascette_formats::CascFormat;
use cascette_formats::blte::BlteFile;
use cascette_protocol::ContentType;

use crate::cdn::CdnSession;
use crate::resolve::Resolver;

pub async fn download_file(session: &CdnSession, ekey: &EncodingKey) -> Result<Vec<u8>> {
    let key_bytes = ekey.as_bytes();
    let raw = session
        .cdn_client()
        .download(session.endpoint(), ContentType::Data, key_bytes)
        .await
        .context("failed to download file from CDN")?;

    let blte = BlteFile::parse(&raw)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("failed to parse BLTE container")?;
    blte.decompress()
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("failed to decompress BLTE data")
}

pub fn save_file(data: &[u8], output_dir: &Path, filename: &str) -> Result<()> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("failed to create output dir: {}", output_dir.display()))?;

    let dest = output_dir.join(filename);
    std::fs::write(&dest, data).with_context(|| format!("failed to write {}", dest.display()))?;

    println!("Saved {} ({} bytes)", dest.display(), data.len());
    Ok(())
}

pub async fn download_and_save(
    session: &CdnSession,
    ekey: &EncodingKey,
    output_path: &Path,
) -> Result<()> {
    let data = download_file(session, ekey).await?;
    let dir = output_path.parent().unwrap_or(Path::new("."));
    let filename = output_path
        .file_name()
        .and_then(|n| n.to_str())
        .context("output_path has no filename")?;
    save_file(&data, dir, filename)
}

pub async fn download_with_deps(
    session: &CdnSession,
    resolver: &Resolver,
    path: &str,
    output_dir: &Path,
) -> Result<()> {
    download_single(session, resolver, path, output_dir).await?;

    if let Some(skin_path) = m2_skin_path(path) {
        eprintln!("Downloading companion skin: {skin_path}");
        match download_single(session, resolver, &skin_path, output_dir).await {
            Ok(()) => {}
            Err(e) => eprintln!("Warning: could not download skin {skin_path}: {e:#}"),
        }
    }

    Ok(())
}

async fn download_single(
    session: &CdnSession,
    resolver: &Resolver,
    path: &str,
    output_dir: &Path,
) -> Result<()> {
    let ekey = resolver.resolve_path(path)?;
    let data = download_file(session, &ekey).await?;
    let filename = basename(path);
    save_file(&data, output_dir, filename)
}

fn m2_skin_path(path: &str) -> Option<String> {
    let lower = path.to_lowercase();
    if !lower.ends_with(".m2") {
        return None;
    }
    let stem = &path[..path.len() - 3];
    Some(format!("{stem}00.skin"))
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}
