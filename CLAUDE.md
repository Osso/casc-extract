# casc-extract

CLI tool for downloading WoW assets from Blizzard's CASC CDN without a local game install.

## Structure

```
src/
├── main.rs       # CLI entry point (clap): init, search, download subcommands
├── cdn.rs        # CdnSession: connect to Blizzard Ribbit/TACT, download build config/root/encoding
├── resolve.rs    # Resolver: map file paths/FDIDs → EncodingKey via root+encoding+listfile
└── download.rs   # Download BLTE-encoded files from CDN, decompress, save. Auto-downloads .skin for .m2
```

## Dependencies

- `cascette-protocol` — RibbitTactClient, CdnClient (local path dep)
- `cascette-formats` — BuildConfig, RootFile, EncodingFile, BlteFile parsing (local path dep)
- `cascette-client-storage` — ContentResolver for path→key resolution (local path dep)
- `cascette-crypto` — ContentKey, EncodingKey, FileDataId types (local path dep)
- `clap` — CLI argument parsing
- `tokio` — async runtime
- `anyhow` — error handling
- `hex` — hex encoding/decoding

## Dev

- `cargo run -- init` — Initialize cache (download root+encoding+listfile)
- `cargo run -- search <pattern>` — Search listfile
- `cargo run -- download <path>` — Download asset by WoW path
- `cargo run -- download --fdid <id>` — Download asset by FileDataID
- `./run-tests.sh` — fmt + clippy + test
- Edition 2024, rust-version 1.89

## CASC Extraction Flow

1. `CdnSession::connect()` — query Ribbit for CDN endpoints + version info
2. `CdnSession::init()` — download BuildConfig → extract root/encoding keys → download+cache both
3. `Resolver::from_cached()` — load root+encoding from cache, build lookup tables
4. `Resolver::resolve_path()` — path → ContentKey (root) → EncodingKey (encoding)
5. `download_file()` — EncodingKey → CDN download → BLTE decompress → raw bytes

## Related

- wow-engine: `../wow-engine/` — Bevy 3D engine for rendering WoW assets
- cascette-rs: `~/Repos/cascette-rs/` — Rust CASC/NGDP protocol implementation
