# Reader Pipeline Benchmarks

Use `bench_reader` with a large CBZ/ZIP or an image folder to measure the first performance targets:

```shell
cargo run --release -p reader-app --bin bench_reader -- /path/to/100mb-comic.cbz
```

The benchmark prints:

- `first_page_ms`: time from process start to decoded page 1.
- `neighbor_page_ms`: time to navigate to page 2.
- `far_jump_ms`: time to jump to the last page.
- cache byte counters emitted by the reader core.

This intentionally uses the same archive, scheduler, decoder, and cache path as the egui prototype.
