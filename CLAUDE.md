# casc-extract

CLI tool to download WoW assets from Blizzard's CASC CDN using cascette-rs.

## Structure

```
src/
├── main.rs       # CLI entry point (clap): init, search, download subcommands
├── cdn.rs        # CDN session: connect, download root/encoding, cache
├── resolve.rs    # ContentResolver wrapper + listfile search
└── download.rs   # Download + BLTE decompress + save to disk
```

## Dependencies

- `cascette-protocol` — RibbitTactClient, CdnClient (from ~/Repos/cascette-rs)
- `cascette-formats` — BlteFile, BuildConfig, BpsvDocument
- `cascette-client-storage` — ContentResolver
- `cascette-crypto` — ContentKey, EncodingKey
- `clap = "4"` — CLI parsing
- `tokio = "1"` — async runtime
- `hex`, `anyhow`

## Usage

```bash
casc-extract init                                    # Download root+encoding+listfile, cache
casc-extract search <pattern>                        # Search listfile by path pattern
casc-extract download <path> [-o dir]                # Download by WoW path
casc-extract download --fdid <id> [-o dir]           # Download by FileDataID
casc-extract download <path> --with-deps [-o dir]    # Download M2 + companion .skin
```

Default output: `./assets/`

## Cache

`~/.cache/casc-extract/`:
- `build-id.txt` — current build ID
- `wow-{build_id}/root.bin` — cached root file
- `wow-{build_id}/encoding.bin` — cached encoding file
- `listfile.csv` — community listfile (~400MB)

## Dev

- `cargo run -- <subcommand>` — Run
- `./run-tests.sh` — fmt + clippy + test
- Edition 2024, rust-version 1.89

## Related

- wow-engine: `../wow-engine/` — Bevy 3D engine consuming downloaded assets
- wow-ui-sim: `../wow-ui-sim/` — WoW addon UI simulator
- cascette-rs: `~/Repos/cascette-rs` — Rust CASC/NGDP protocol crates
