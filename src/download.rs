use std::path::Path;

use anyhow::{Context, Result};
use cascette_crypto::EncodingKey;

use crate::archive::ArchiveManager;
use crate::cdn::CdnSession;
use crate::resolve::Resolver;

pub async fn download_file(
    archive_mgr: &ArchiveManager,
    cdn: &CdnSession,
    ekey: &EncodingKey,
) -> Result<Vec<u8>> {
    archive_mgr.download(cdn, ekey.as_bytes()).await
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
    archive_mgr: &ArchiveManager,
    cdn: &CdnSession,
    ekey: &EncodingKey,
    output_path: &Path,
) -> Result<()> {
    let data = download_file(archive_mgr, cdn, ekey).await?;
    let dir = output_path.parent().unwrap_or(Path::new("."));
    let filename = output_path
        .file_name()
        .and_then(|n| n.to_str())
        .context("output_path has no filename")?;
    save_file(&data, dir, filename)
}

pub async fn download_with_deps(
    archive_mgr: &ArchiveManager,
    cdn: &CdnSession,
    resolver: &Resolver,
    path: &str,
    output_dir: &Path,
) -> Result<()> {
    download_single(archive_mgr, cdn, resolver, path, output_dir).await?;

    let effective = effective_path_for(resolver, path);
    let skin_source = effective.as_deref().unwrap_or(path);

    if let Some(skin_path) = m2_skin_path(skin_source) {
        eprintln!("Downloading companion skin: {skin_path}");
        match download_single(archive_mgr, cdn, resolver, &skin_path, output_dir).await {
            Ok(()) => {}
            Err(e) => eprintln!("Warning: could not download skin {skin_path}: {e:#}"),
        }
    }

    Ok(())
}

fn effective_path_for(resolver: &Resolver, path: &str) -> Option<String> {
    let id_str = path.strip_prefix("__fdid:")?;
    let fdid: u32 = id_str.parse().ok()?;
    resolver.path_for_fdid(fdid).map(|s| s.to_string())
}

async fn download_single(
    archive_mgr: &ArchiveManager,
    cdn: &CdnSession,
    resolver: &Resolver,
    path: &str,
    output_dir: &Path,
) -> Result<()> {
    let (ekey, filename) = resolve_ekey_and_filename(resolver, path)?;
    let data = download_file(archive_mgr, cdn, &ekey).await?;
    save_file(&data, output_dir, &filename)
}

fn resolve_ekey_and_filename(
    resolver: &Resolver,
    path: &str,
) -> Result<(EncodingKey, String)> {
    if let Some(id_str) = path.strip_prefix("__fdid:") {
        let fdid: u32 = id_str.parse().context("invalid fdid")?;
        let ekey = resolver.resolve_fdid(fdid)?;
        let filename = resolver
            .path_for_fdid(fdid)
            .and_then(|p| p.rsplit(['/', '\\']).next())
            .unwrap_or(id_str)
            .to_string();
        Ok((ekey, filename))
    } else {
        let ekey = resolver.resolve_path(path)?;
        let filename = basename(path).to_string();
        Ok((ekey, filename))
    }
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
