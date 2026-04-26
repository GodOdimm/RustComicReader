use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use archive::backend_for_path;
use eframe::egui;
use image_pipeline::ImageCrateDecoder;
use reader_core::{spawn_reader, DecodedImage, ReaderEvent, ReaderHandle, ReaderOptions};

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1200.0, 900.0]),
        ..Default::default()
    };

    eframe::run_native(
        "RustComicReader",
        options,
        Box::new(|creation_context| Ok(Box::new(ComicReaderApp::new(creation_context)))),
    )
}

#[derive(Default)]
pub struct ComicReaderApp {
    handle: Option<ReaderHandle>,
    current_path: Option<PathBuf>,
    path_input: String,
    current_page: usize,
    page_count: usize,
    status: String,
    cache_status: String,
    textures: HashMap<usize, egui::TextureHandle>,
    decoded_images: HashMap<usize, Arc<DecodedImage>>,
    image_sizes: HashMap<usize, [usize; 2]>,
    thumbnails: HashMap<usize, egui::TextureHandle>,
    last_thumbnail_request: Option<(usize, usize)>,
    show_thumbnails: bool,
}

impl ComicReaderApp {
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        install_system_fonts(&creation_context.egui_ctx);

        Self {
            status: "打开一个 CBZ/ZIP 漫画或图片文件夹开始阅读".to_string(),
            ..Default::default()
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        self.textures.clear();
        self.decoded_images.clear();
        self.image_sizes.clear();
        self.thumbnails.clear();
        self.last_thumbnail_request = None;
        self.current_page = 0;
        self.page_count = 0;
        self.cache_status.clear();
        self.status = format!("正在打开 {}", path.display());

        match backend_for_path(&path) {
            Ok(backend) => {
                let decoder = Arc::new(ImageCrateDecoder::new());
                self.handle = Some(spawn_reader(backend, decoder, ReaderOptions::default()));
                self.current_path = Some(path);
            }
            Err(error) => {
                self.handle = None;
                self.status = error.to_string();
            }
        }
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        let Some(handle) = self.handle.clone() else {
            return;
        };

        while let Some(event) = handle.try_recv() {
            match event {
                ReaderEvent::PageCount { pages } => {
                    self.page_count = pages;
                    self.status = format!("共 {pages} 页");
                }
                ReaderEvent::CurrentPage { page_index } => {
                    self.current_page = page_index;
                    self.status = format!("第 {} / {} 页", page_index + 1, self.page_count.max(1));
                    self.upload_nearby_textures(ctx);
                }
                ReaderEvent::LoadingPage { page_index } => {
                    if page_index == self.current_page {
                        self.status = format!("正在加载第 {} 页", page_index + 1);
                    }
                }
                ReaderEvent::PageDecoded {
                    page_index,
                    image,
                    from_cache,
                    elapsed_ms,
                } => {
                    self.image_sizes
                        .insert(page_index, [image.width as usize, image.height as usize]);
                    self.decoded_images.insert(page_index, image.clone());
                    if self.should_keep_texture(page_index) {
                        self.upload_texture_for_page(ctx, page_index);
                    }
                    self.trim_page_textures();

                    if page_index == self.current_page {
                        let source = if from_cache { "缓存" } else { "解码" };
                        self.status = format!(
                            "第 {} / {} 页，{}耗时 {}ms",
                            page_index + 1,
                            self.page_count.max(1),
                            source,
                            elapsed_ms
                        );
                    }
                }
                ReaderEvent::CacheStats {
                    raw_bytes,
                    display_bytes,
                    thumbnail_bytes,
                } => {
                    self.cache_status = format!(
                        "raw {} | display {} | thumb {}",
                        format_bytes(raw_bytes),
                        format_bytes(display_bytes),
                        format_bytes(thumbnail_bytes)
                    );
                }
                ReaderEvent::Error(error) => {
                    self.status = error;
                }
                ReaderEvent::ThumbnailDecoded { page_index, image } => {
                    let texture = texture_from_image(ctx, "thumb", page_index, &image);
                    self.thumbnails.insert(page_index, texture);
                }
                ReaderEvent::Finished => {}
            }
        }
    }

    fn upload_texture_for_page(&mut self, ctx: &egui::Context, page_index: usize) {
        if self.textures.contains_key(&page_index) {
            return;
        }

        if let Some(image) = self.decoded_images.get(&page_index) {
            let texture = texture_from_image(ctx, "page", page_index, image);
            self.textures.insert(page_index, texture);
        }
    }

    fn upload_nearby_textures(&mut self, ctx: &egui::Context) {
        for page in self.texture_window() {
            self.upload_texture_for_page(ctx, page);
        }
        self.trim_page_textures();
    }

    fn should_keep_texture(&self, page_index: usize) -> bool {
        self.current_page.abs_diff(page_index) <= 1
    }

    fn texture_window(&self) -> impl Iterator<Item = usize> {
        let start = self.current_page.saturating_sub(1);
        let end = (self.current_page + 1).min(self.page_count.saturating_sub(1));
        start..=end
    }

    fn trim_page_textures(&mut self) {
        let current = self.current_page;
        self.textures.retain(|page, _| current.abs_diff(*page) <= 1);
    }

    fn request_visible_thumbnails(&mut self) {
        const THUMB_RADIUS: usize = 8;

        let Some(handle) = &self.handle else {
            return;
        };

        if self.page_count == 0 || !self.show_thumbnails {
            return;
        }

        let request = (self.current_page, THUMB_RADIUS);
        if self.last_thumbnail_request == Some(request) {
            return;
        }

        handle.request_thumbnails(self.current_page, THUMB_RADIUS);
        self.last_thumbnail_request = Some(request);
    }

    fn update_thumbnail_visibility(&mut self, ctx: &egui::Context) {
        const HOT_ZONE_HEIGHT: f32 = 36.0;
        const HIDE_ABOVE_BOTTOM: f32 = 140.0;

        let Some(pointer) = ctx.pointer_hover_pos() else {
            self.show_thumbnails = false;
            return;
        };

        let bottom = ctx.content_rect().bottom();
        if pointer.y >= bottom - HOT_ZONE_HEIGHT {
            self.show_thumbnails = true;
        } else if pointer.y < bottom - HIDE_ABOVE_BOTTOM {
            self.show_thumbnails = false;
        }
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("路径");
            let response = ui.add(
                egui::TextEdit::singleline(&mut self.path_input)
                    .desired_width(420.0)
                    .hint_text("/path/to/comic.cbz 或图片文件夹"),
            );
            let open_requested = ui.button("打开").clicked()
                || (response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)));

            if open_requested && !self.path_input.trim().is_empty() {
                self.open_path(PathBuf::from(self.path_input.trim()));
            }

            ui.separator();

            let can_read = self.handle.is_some() && self.page_count > 0;
            if ui
                .add_enabled(
                    can_read && self.current_page > 0,
                    egui::Button::new("上一页"),
                )
                .clicked()
            {
                if let Some(handle) = &self.handle {
                    handle.previous();
                }
            }

            if ui
                .add_enabled(
                    can_read && self.current_page + 1 < self.page_count,
                    egui::Button::new("下一页"),
                )
                .clicked()
            {
                if let Some(handle) = &self.handle {
                    handle.next();
                }
            }

            if can_read {
                let mut page = self.current_page + 1;
                let response = ui.add(
                    egui::DragValue::new(&mut page)
                        .range(1..=self.page_count)
                        .speed(1),
                );
                if response.changed() {
                    if let Some(handle) = &self.handle {
                        handle.go_to(page.saturating_sub(1));
                    }
                }
                ui.label(format!("/ {}", self.page_count));
                ui.separator();
                ui.label("方向键左右翻页");
            }
        });
    }

    fn handle_keyboard_shortcuts(&self, ctx: &egui::Context) {
        if self.handle.is_none() || self.page_count == 0 || ctx.egui_wants_keyboard_input() {
            return;
        }

        let previous = ctx.input(|input| input.key_pressed(egui::Key::ArrowLeft));
        let next = ctx.input(|input| input.key_pressed(egui::Key::ArrowRight));

        if previous && self.current_page > 0 {
            if let Some(handle) = &self.handle {
                handle.previous();
            }
        } else if next && self.current_page + 1 < self.page_count {
            if let Some(handle) = &self.handle {
                handle.next();
            }
        }
    }

    fn image_panel(&mut self, ui: &mut egui::Ui) {
        let Some(texture) = self.textures.get(&self.current_page) else {
            ui.centered_and_justified(|ui| {
                ui.label(&self.status);
            });
            return;
        };

        let available = ui.available_size();
        let image_size = self
            .image_sizes
            .get(&self.current_page)
            .map(|[width, height]| egui::vec2(*width as f32, *height as f32))
            .unwrap_or_else(|| texture.size_vec2());
        let fit = fit_size(image_size, available);

        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add(egui::Image::new((texture.id(), fit)));
                });
            });
    }

    fn thumbnail_strip(&mut self, ui: &mut egui::Ui) {
        if self.page_count == 0 {
            return;
        }

        const THUMB_RADIUS: usize = 8;
        let start = self.current_page.saturating_sub(THUMB_RADIUS);
        let end = (self.current_page + THUMB_RADIUS).min(self.page_count - 1);

        egui::ScrollArea::horizontal()
            .id_salt("thumbnail-strip")
            .max_height(92.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if start > 0 && ui.button("<<").clicked() {
                        if let Some(handle) = &self.handle {
                            handle.go_to(start.saturating_sub(THUMB_RADIUS));
                        }
                    }

                    for page in start..=end {
                        let selected = page == self.current_page;
                        let fill = if selected {
                            ui.visuals().selection.bg_fill
                        } else {
                            ui.visuals().faint_bg_color
                        };

                        egui::Frame::new()
                            .fill(fill)
                            .inner_margin(egui::Margin::same(3))
                            .show(ui, |ui| {
                                let response = if let Some(texture) = self.thumbnails.get(&page) {
                                    let size =
                                        fit_size(texture.size_vec2(), egui::vec2(54.0, 76.0));
                                    ui.add(
                                        egui::Image::new((texture.id(), size))
                                            .sense(egui::Sense::click()),
                                    )
                                } else {
                                    ui.add_sized(
                                        [54.0, 76.0],
                                        egui::Button::new(format!("{}", page + 1)),
                                    )
                                };

                                if response.clicked() {
                                    if let Some(handle) = &self.handle {
                                        handle.go_to(page);
                                    }
                                }
                            });
                    }

                    if end + 1 < self.page_count && ui.button(">>").clicked() {
                        if let Some(handle) = &self.handle {
                            handle.go_to((end + THUMB_RADIUS).min(self.page_count - 1));
                        }
                    }
                });
            });
    }
}

impl eframe::App for ComicReaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_events(&ctx);
        self.handle_keyboard_shortcuts(&ctx);
        self.update_thumbnail_visibility(&ctx);
        self.request_visible_thumbnails();

        ui.vertical(|ui| {
            self.top_bar(ui);
            ui.separator();

            let thumbnail_height = if self.show_thumbnails && self.page_count > 0 {
                98.0
            } else {
                0.0
            };
            let image_height = (ui.available_height() - thumbnail_height - 28.0).max(0.0);
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), image_height),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    self.image_panel(ui);
                },
            );

            if self.show_thumbnails && self.page_count > 0 {
                ui.separator();
                self.thumbnail_strip(ui);
            }

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.status);
                if !self.cache_status.is_empty() {
                    ui.separator();
                    ui.label(&self.cache_status);
                }
                if let Some(path) = &self.current_path {
                    ui.separator();
                    ui.label(path.display().to_string());
                }
            });
        });

        if self.handle.is_some() {
            ui.ctx().request_repaint();
        }
    }
}

fn texture_from_image(
    ctx: &egui::Context,
    prefix: &str,
    page_index: usize,
    image: &DecodedImage,
) -> egui::TextureHandle {
    let color_image = egui::ColorImage::from_rgba_unmultiplied(
        [image.width as usize, image.height as usize],
        &image.rgba,
    );

    ctx.load_texture(
        format!("{prefix}-{page_index}"),
        color_image,
        egui::TextureOptions::LINEAR,
    )
}

fn install_system_fonts(ctx: &egui::Context) {
    let candidates = [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "/System/Library/Fonts/Supplemental/Songti.ttc",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    ];

    let Some(font_bytes) = candidates.iter().find_map(|path| fs::read(path).ok()) else {
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    let font_name = "system-cjk".to_string();
    fonts.font_data.insert(
        font_name.clone(),
        Arc::new(egui::FontData::from_owned(font_bytes)),
    );

    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, font_name.clone());
    }

    ctx.set_fonts(fonts);
}

fn fit_size(image_size: egui::Vec2, available: egui::Vec2) -> egui::Vec2 {
    if image_size.x <= 0.0 || image_size.y <= 0.0 || available.x <= 0.0 || available.y <= 0.0 {
        return image_size;
    }

    let scale = (available.x / image_size.x).min(available.y / image_size.y);
    image_size * scale.min(1.0)
}

fn format_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;

    if bytes as f64 >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes as f64 >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

#[allow(dead_code)]
fn is_supported_open_path(path: &Path) -> bool {
    path.is_dir()
        || matches!(
            path.extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| extension.to_ascii_lowercase())
                .as_deref(),
            Some("cbz" | "zip")
        )
}
