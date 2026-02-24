mod cdn;
mod download;
mod resolve;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::resolve::Resolver;

#[derive(Parser)]
#[command(name = "casc-extract", about = "Download WoW assets from Blizzard CDN")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download root+encoding+listfile and cache locally
    Init,
    /// Search listfile by path pattern
    Search { pattern: String },
    /// Download a file by WoW path or FileDataID
    Download {
        /// WoW asset path (e.g. "Item/ObjectComponents/Backpack/Backpack.m2")
        path: Option<String>,
        /// Download by FileDataID instead of path
        #[arg(long)]
        fdid: Option<u32>,
        /// Output directory
        #[arg(short, long, default_value = "./assets")]
        output: PathBuf,
        /// Also download companion .skin file for M2 models
        #[arg(long)]
        with_deps: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Init => cmd_init().await,
        Command::Search { pattern } => cmd_search(&pattern),
        Command::Download {
            path,
            fdid,
            output,
            with_deps,
        } => cmd_download(path, fdid, output, with_deps).await,
    }
}

async fn cmd_init() -> Result<()> {
    eprintln!("Connecting to Blizzard CDN...");
    let session = cdn::CdnSession::connect()
        .await
        .context("failed to connect to CDN")?;

    eprintln!("Downloading root + encoding...");
    let cache_dir = session.init().await?;
    eprintln!("Cached to: {}", cache_dir.display());

    eprintln!("Downloading listfile...");
    let listfile_path = resolve::download_listfile().await?;
    eprintln!("Listfile cached to: {}", listfile_path.display());

    save_build_id(
        session
            .cache_dir()
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("wow-"))
            .context("unexpected cache dir name format")?,
    )?;

    eprintln!("Init complete.");
    Ok(())
}

fn cmd_search(pattern: &str) -> Result<()> {
    let build_id = load_build_id()?;
    let (root, encoding) = cdn::load_cached(&build_id)?;
    let mut resolver = Resolver::from_cached(&root, &encoding)?;
    resolver.load_listfile(&resolve::listfile_cache_path())?;

    let results = resolver.search(pattern);
    if results.is_empty() {
        eprintln!("No results for pattern: {pattern}");
    }
    for (fdid, path) in results {
        println!("{fdid}\t{path}");
    }
    Ok(())
}

async fn cmd_download(
    path: Option<String>,
    fdid: Option<u32>,
    output: PathBuf,
    with_deps: bool,
) -> Result<()> {
    let wow_path = resolve_download_target(path, fdid)?;

    let build_id = load_build_id()?;
    eprintln!("Connecting to Blizzard CDN...");
    let session = cdn::CdnSession::connect()
        .await
        .context("failed to connect to CDN")?;

    let (root, encoding) = cdn::load_cached(&build_id)?;
    let mut resolver = Resolver::from_cached(&root, &encoding)?;
    resolver.load_listfile(&resolve::listfile_cache_path())?;

    if with_deps {
        eprintln!("Downloading {wow_path} with dependencies...");
        download::download_with_deps(&session, &resolver, &wow_path, &output).await?;
    } else {
        download_by_path(&session, &resolver, &wow_path, &output).await?;
    }

    Ok(())
}

fn resolve_download_target(path: Option<String>, fdid: Option<u32>) -> Result<String> {
    match (path, fdid) {
        (Some(_), Some(_)) => anyhow::bail!("specify either <path> or --fdid, not both"),
        (None, None) => anyhow::bail!("specify either <path> or --fdid"),
        (Some(p), None) => Ok(p),
        (None, Some(id)) => Ok(format!("__fdid:{id}")),
    }
}

async fn download_by_path(
    session: &cdn::CdnSession,
    resolver: &Resolver,
    wow_path: &str,
    output: &Path,
) -> Result<()> {
    if let Some(id_str) = wow_path.strip_prefix("__fdid:") {
        let fdid: u32 = id_str.parse().context("invalid fdid")?;
        let ekey = resolver.resolve_fdid(fdid)?;
        let filename = resolver
            .path_for_fdid(fdid)
            .and_then(|p| p.rsplit(['/', '\\']).next())
            .unwrap_or(id_str);
        eprintln!("Downloading fdid={fdid} as {filename}...");
        let dest = output.join(filename);
        download::download_and_save(session, &ekey, &dest).await?;
    } else {
        let ekey = resolver.resolve_path(wow_path)?;
        let filename = wow_path.rsplit(['/', '\\']).next().unwrap_or(wow_path);
        eprintln!("Downloading {wow_path}...");
        let dest = output.join(filename);
        download::download_and_save(session, &ekey, &dest).await?;
    }
    Ok(())
}

fn build_id_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".cache")
        .join("casc-extract")
        .join("build-id.txt")
}

fn save_build_id(build_id: &str) -> Result<()> {
    let path = build_id_file();
    std::fs::create_dir_all(path.parent().expect("build-id path has parent"))
        .context("failed to create cache dir")?;
    std::fs::write(&path, build_id)
        .with_context(|| format!("failed to write build id to {}", path.display()))?;
    eprintln!("Build ID saved: {build_id}");
    Ok(())
}

fn load_build_id() -> Result<String> {
    let path = build_id_file();
    std::fs::read_to_string(&path).with_context(|| {
        format!(
            "failed to read build ID from {} — run `casc-extract init` first",
            path.display()
        )
    })
}
