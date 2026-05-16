# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## Project Overview

RustComicReader is a Rust rewrite of the YACReader reader module. It is a high-performance comic reader supporting CBZ/ZIP archives and image folders, with priority-based page decoding, LRU caching, and an egui/wgpu GUI.

## Workspace Structure

This is a Cargo workspace with 5 crates:

| Crate | Purpose |
|---|---|
| `reader-core` | Core traits (`ArchiveBackend`, `ImageDecoder`, `Cache`), page ordering, LRU cache, background reader thread with command/event channels |
| `archive` | ZIP/CBZ (`ZipArchiveBackend`) and folder (`FolderArchiveBackend`) implementations of `ArchiveBackend` |
| `image-pipeline` | Image decoding (`ImageCrateDecoder`) using `image` crate + native `libwebp` for WebP |
| `reader-ui` | egui/wgpu GUI app (`ComicReaderApp`) with image display, thumbnail strip, keyboard navigation |
| `reader-app` | Binary entrypoint (`main.rs`) and performance benchmark (`bench_reader.rs`) |

## Key Architecture

- **Reader thread model**: `reader-core` spawns a background `ReaderWorker` thread that handles all I/O and decoding. The main UI thread communicates via `crossbeam_channel` with `ReaderCommand` (go-to, next, previous, request thumbnails, shutdown) and receives `ReaderEvent` (page decoded, thumbnail decoded, cache stats, errors).
- **Three-layer cache**: raw bytes (LRU, byte-budgeted), decoded images (LRU), thumbnails (LRU). Default budgets: 256MB raw, 512MB display, 64MB thumbnails.
- **Prefetch strategy**: on page change, builds a prioritized window (current page, then alternating forward/backward). Thumbnails are interleaved every 2 prefetches to avoid starving the thumbnail queue.
- **Page ordering**: natural sort (`natord`) of filenames, filtering to supported image extensions (jpg, jpeg, png, webp, gif, bmp, tif, tiff).

## Common Commands

```shell
# Run the GUI reader
cargo run -p reader-app

# Run with a specific comic file
cargo run -p reader-app -- /path/to/comic.cbz

# Run performance benchmark (release mode)
cargo run --release -p reader-app --bin bench_reader -- /path/to/comic.cbz

# Run tests
cargo test

# Run tests for a specific crate
cargo test -p reader-core
```

## Toolchain

- Rust stable (pinned via `rust-toolchain.toml`)
- `dev` profile: opt-level 1 for workspace, opt-level 3 for dependencies

## Dependencies to Know

- `egui`/`eframe` 0.34.1 with wgpu for GUI rendering
- `image` 0.25.5 for general image decoding (jpg, png, gif, bmp, tiff)
- `libwebp` 0.3.0 for fast native WebP decoding
- `zip` 2.4.2 (deflate only) for CBZ/ZIP archive access
- `crossbeam-channel` for thread communication
- `rfd` for file open dialogs (macOS native)

## Cursor Rules

The project has a Cursor rule (`.cursor/rules/project-context.mdc`) stating that references to "原项目" (original project) mean the YACReader reader module at `/Users/chenfeng/CLionProjects/yacreader`.
