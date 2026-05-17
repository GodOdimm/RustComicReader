# RustComicReader

RustComicReader 是对项目 [YACReader](https://github.com/YACReader) 阅读器模块的 Rust 重构原型，当前重点是验证高性能漫画阅读管线：按条目读取 CBZ/ZIP、当前页优先、后台解码、固定窗口缓存和 egui/wgpu 显示。

## 运行阅读器原型

项目使用 `rust-toolchain.toml` 固定到 `stable`。当前 GUI 依赖需要新版 Rust；如果本机默认还是旧工具链，进入项目目录后 Cargo 会自动使用 stable。

```shell
cargo run -p reader-app
```

启动后会全屏显示。按 `O` 键打开 macOS 文件选择框并选择 CBZ/ZIP 漫画。

也可以在启动时传入漫画路径，适合由其他项目调用：

```shell
cargo run -p reader-app -- /path/to/comic.cbz
```

## 运行性能压测

```shell
cargo run --release -p reader-app --bin bench_reader -- /path/to/100mb-comic.cbz
```

压测会输出首屏、邻近页、远距离跳页耗时，并打印缓存占用。

## 本地打包 macOS DMG

项目提供本地使用的 unsigned DMG 打包脚本。脚本会运行测试、生成 release `.app`、做 ad-hoc 签名，并输出 DMG：

```shell
./scripts/package-macos.sh
```

输出文件位于：

```shell
dist/RustComicReader-1.0.0-macos.dmg
```

这个 DMG 适合个人本机使用；如果要公开分发，需要再使用 Apple Developer ID 证书签名并提交 Apple notarization。

## Workspace 结构

- `crates/reader-core`：阅读器核心 trait、页面排序、LRU 缓存、后台调度和事件流。
- `crates/archive`：ZIP/CBZ 与图片文件夹 backend。
- `crates/image-pipeline`：图片解码和缩略图生成。
- `crates/reader-ui`：egui/wgpu 最小阅读 UI。
- `crates/reader-app`：应用入口和压测命令。
