use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use casc_extract::archive::ArchiveManager;
use casc_extract::cdn::{self, Product};
use casc_extract::download;
use casc_extract::resolve::{self, Resolver};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "casc-extract", about = "Download WoW assets from Blizzard CDN")]
struct Cli {
    /// Blizzard product code, for example "wow" or "wowt"
    #[arg(long, default_value = "wow")]
    product: String,
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
    /// Diagnostic: check root file stats
    Diag,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let product = Product::new(cli.product)?;
    match cli.command {
        Command::Init => cmd_init(&product).await,
        Command::Search { pattern } => cmd_search(&product, &pattern),
        Command::Download {
            path,
            fdid,
            output,
            with_deps,
        } => cmd_download(&product, path, fdid, output, with_deps).await,
        Command::Diag => cmd_diag(&product),
    }
}

async fn cmd_init(product: &Product) -> Result<()> {
    eprintln!(
        "Connecting to Blizzard CDN for product {}...",
        product.name()
    );
    let session = cdn::CdnSession::connect_product(product.clone())
        .await
        .context("failed to connect to CDN")?;

    eprintln!("Downloading root + encoding...");
    let cache_dir = session.init().await?;
    eprintln!("Cached to: {}", cache_dir.display());

    eprintln!("Downloading listfile...");
    let listfile_path = resolve::download_listfile().await?;
    eprintln!("Listfile cached to: {}", listfile_path.display());

    eprintln!("Downloading archive indices...");
    ArchiveManager::init(&session)
        .await
        .context("failed to initialise archive indices")?;
    eprintln!("Archive indices cached.");

    save_build_id(product, session.build_id())?;

    eprintln!("Init complete.");
    Ok(())
}

fn cmd_search(product: &Product, pattern: &str) -> Result<()> {
    let build_id = load_build_id(product)?;
    let (root, encoding) = cdn::load_cached_product(product, &build_id)?;
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
    product: &Product,
    path: Option<String>,
    fdid: Option<u32>,
    output: PathBuf,
    with_deps: bool,
) -> Result<()> {
    let wow_path = resolve_download_target(path, fdid)?;

    let build_id = load_build_id(product)?;
    eprintln!(
        "Connecting to Blizzard CDN for product {}...",
        product.name()
    );
    let session = cdn::CdnSession::connect_product(product.clone())
        .await
        .context("failed to connect to CDN")?;

    let cache_dir = session.cache_dir();
    let archive_mgr = ArchiveManager::load_cached(&cache_dir)
        .context("failed to load cached archive indices — run `casc-extract init` first")?;

    let (root, encoding) = cdn::load_cached_product(product, &build_id)?;
    let mut resolver = Resolver::from_cached(&root, &encoding)?;
    resolver.load_listfile(&resolve::listfile_cache_path())?;

    if with_deps {
        eprintln!("Downloading {wow_path} with dependencies...");
        download::download_with_deps(&archive_mgr, &session, &resolver, &wow_path, &output).await?;
    } else {
        download_by_path(&archive_mgr, &session, &resolver, &wow_path, &output).await?;
    }

    Ok(())
}

fn cmd_diag(product: &Product) -> Result<()> {
    use cascette_formats::root::RootFile;

    let build_id = load_build_id(product)?;
    eprintln!("Build ID: {build_id}");
    let (root, encoding) = cdn::load_cached_product(product, &build_id)?;
    eprintln!("Root file: {} bytes", root.len());
    eprintln!("Encoding file: {} bytes", encoding.len());

    let resolver = Resolver::from_cached(&root, &encoding)?;
    diag_probe_fdids(&resolver);

    let root_file = RootFile::parse(&root).expect("parse root");
    diag_print_root_stats(&root_file);
    diag_probe_root_resolve(&root_file);
    diag_sample_fdids(&root_file);
    diag_resolve_first_fdid(&root_file, &resolver);

    Ok(())
}

fn diag_probe_fdids(resolver: &Resolver) {
    for fdid in [125024u32, 1011653, 1, 100, 1000, 10000, 100000, 500000] {
        let found = resolver.resolve_fdid(fdid).is_ok();
        eprintln!("FDID {fdid}: {}", if found { "FOUND" } else { "not found" });
    }
}

fn diag_print_root_stats(root_file: &cascette_formats::root::RootFile) {
    eprintln!("Root version: {:?}", root_file.version);
    eprintln!("Root total_files: {}", root_file.total_files());
    eprintln!("Root named_files: {}", root_file.named_files());
    eprintln!("Root blocks: {}", root_file.num_blocks());
    let (fdid_count, name_count) = root_file.lookup_stats();
    eprintln!("Root lookup stats: fdid={fdid_count}, name={name_count}");
}

fn diag_probe_root_resolve(root_file: &cascette_formats::root::RootFile) {
    use cascette_crypto::md5::FileDataId;
    use cascette_formats::root::flags::{ContentFlags, LocaleFlags};

    let content = ContentFlags::new(0);
    let locale = LocaleFlags::new(LocaleFlags::ENUS);
    for fdid in [125024u32, 1011653] {
        let found = root_file
            .resolve_by_id(FileDataId::new(fdid), locale, content)
            .is_some();
        eprintln!(
            "Direct resolve FDID {fdid}: {}",
            if found { "FOUND" } else { "not found" }
        );
    }
    let locale_all = LocaleFlags::new(LocaleFlags::ALL);
    for fdid in [125024u32, 1011653] {
        let found = root_file
            .resolve_by_id(FileDataId::new(fdid), locale_all, content)
            .is_some();
        eprintln!(
            "Direct resolve (ALL locale) FDID {fdid}: {}",
            if found { "FOUND" } else { "not found" }
        );
    }
}

fn diag_sample_fdids(root_file: &cascette_formats::root::RootFile) {
    eprintln!("\nSampling first 10 FDIDs from root file blocks:");
    let mut count = 0;
    'outer: for block in &root_file.blocks {
        for record in &block.records {
            eprintln!("  Block FDID: {}", record.file_data_id.get());
            count += 1;
            if count >= 10 {
                break 'outer;
            }
        }
    }
}

fn diag_resolve_first_fdid(root_file: &cascette_formats::root::RootFile, resolver: &Resolver) {
    if let Some(first_record) = root_file.blocks.first().and_then(|b| b.records.first()) {
        let test_fdid = first_record.file_data_id.get();
        eprintln!("\nTrying to resolve first FDID {test_fdid} via ContentResolver...");
        let found = resolver.resolve_fdid(test_fdid).is_ok();
        eprintln!("Result: {}", if found { "FOUND" } else { "not found" });
    }
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
    archive_mgr: &ArchiveManager,
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
        download::download_and_save(archive_mgr, session, &ekey, &dest).await?;
    } else {
        let ekey = resolver.resolve_path(wow_path)?;
        let filename = wow_path.rsplit(['/', '\\']).next().unwrap_or(wow_path);
        eprintln!("Downloading {wow_path}...");
        let dest = output.join(filename);
        download::download_and_save(archive_mgr, session, &ekey, &dest).await?;
    }
    Ok(())
}

fn build_id_file(product: &Product) -> PathBuf {
    let filename = if product.name() == "wow" {
        "build-id.txt".to_string()
    } else {
        format!("build-id-{}.txt", product.name())
    };
    cdn::default_cache_root().join(filename)
}

fn save_build_id(product: &Product, build_id: &str) -> Result<()> {
    let path = build_id_file(product);
    std::fs::create_dir_all(path.parent().expect("build-id path has parent"))
        .context("failed to create cache dir")?;
    std::fs::write(&path, build_id)
        .with_context(|| format!("failed to write build id to {}", path.display()))?;
    eprintln!("Build ID saved for product {}: {build_id}", product.name());
    Ok(())
}

fn load_build_id(product: &Product) -> Result<String> {
    let path = build_id_file(product);
    std::fs::read_to_string(&path).with_context(|| {
        format!(
            "failed to read build ID from {} — run `casc-extract --product {} init` first",
            path.display(),
            product.name()
        )
    })
}
