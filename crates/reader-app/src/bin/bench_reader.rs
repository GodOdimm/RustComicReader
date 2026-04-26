use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use archive::backend_for_path;
use image_pipeline::ImageCrateDecoder;
use reader_core::{spawn_reader, ReaderEvent, ReaderHandle, ReaderOptions};

fn main() {
    let Some(path) = env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: cargo run -p reader-app --bin bench_reader -- <comic.cbz|folder>");
        std::process::exit(2);
    };

    let started = Instant::now();
    let backend = match backend_for_path(&path) {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let handle = spawn_reader(
        backend,
        Arc::new(ImageCrateDecoder::new()),
        ReaderOptions::default(),
    );

    let page_count = wait_for_page_count(&handle);
    let first_page_ms = wait_for_page(&handle, 0, Duration::from_secs(30));
    println!(
        "first_page_ms={} page_count={} path={}",
        first_page_ms,
        page_count,
        path.display()
    );

    let neighbor = 1.min(page_count.saturating_sub(1));
    if neighbor > 0 {
        handle.go_to(neighbor);
        let elapsed = wait_for_page(&handle, neighbor, Duration::from_secs(30));
        println!("neighbor_page_ms={elapsed} page={neighbor}");
    }

    if page_count > 2 {
        let far_page = page_count - 1;
        handle.go_to(far_page);
        let elapsed = wait_for_page(&handle, far_page, Duration::from_secs(30));
        println!("far_jump_ms={elapsed} page={far_page}");
    }

    println!("total_elapsed_ms={}", started.elapsed().as_millis());
    handle.shutdown();
}

fn wait_for_page_count(handle: &ReaderHandle) -> usize {
    loop {
        match handle.recv() {
            Some(ReaderEvent::PageCount { pages }) => return pages,
            Some(ReaderEvent::Error(error)) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
            Some(_) => {}
            None => {
                eprintln!("reader stopped before reporting page count");
                std::process::exit(1);
            }
        }
    }
}

fn wait_for_page(handle: &ReaderHandle, page: usize, timeout: Duration) -> u128 {
    let started = Instant::now();

    loop {
        if started.elapsed() > timeout {
            eprintln!("timed out waiting for page {page}");
            std::process::exit(1);
        }

        match handle.recv() {
            Some(ReaderEvent::PageDecoded { page_index, .. }) if page_index == page => {
                return started.elapsed().as_millis();
            }
            Some(ReaderEvent::CacheStats {
                raw_bytes,
                display_bytes,
                thumbnail_bytes,
            }) => {
                eprintln!(
                    "cache raw={} display={} thumb={}",
                    raw_bytes, display_bytes, thumbnail_bytes
                );
            }
            Some(ReaderEvent::Error(error)) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
            Some(_) => {}
            None => {
                eprintln!("reader stopped before decoding page {page}");
                std::process::exit(1);
            }
        }
    }
}
