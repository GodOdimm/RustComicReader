use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use archive::backend_for_path;
use eframe::egui;
use image_pipeline::ImageCrateDecoder;
use reader_core::{spawn_reader, DecodedImage, ReaderEvent, ReaderHandle, ReaderOptions};

const LEGACY_READING_PROGRESS_PATH: &str = "meta/reading_progress.tsv";
const MANHWA_DEFAULT_WIDTH_RATIO: f32 = 0.66;
const MANHWA_NARROW_WIDTH_RATIO: f32 = 0.72;
const MANHWA_MAX_CONTENT_WIDTH: f32 = 1180.0;
const MANHWA_MIN_ZOOM: f32 = 0.40;
const MANHWA_MAX_ZOOM: f32 = 3.00;
const MANHWA_ZOOM_STEP: f32 = 0.10;
const MANHWA_WHEEL_SCROLL_SENSITIVITY: f32 = 0.65;
const MANHWA_PAGE_GAP: f32 = 0.0;
const MANHWA_LOOKAHEAD_PAGES: usize = 3;

pub fn run(initial_path: Option<PathBuf>) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_fullscreen(true)
            .with_icon(app_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "RustComicReader",
        options,
        Box::new(|creation_context| {
            Ok(Box::new(ComicReaderApp::new(
                creation_context,
                initial_path,
            )))
        }),
    )
}

fn app_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../../icon.png"))
        .expect("embedded icon.png must be a valid PNG")
}

#[derive(Default)]
pub struct ComicReaderApp {
    handle: Option<ReaderHandle>,
    current_path: Option<PathBuf>,
    current_progress_key: Option<String>,
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
    flow_center: usize,
    flow_target: usize,
    flow_animated: f32,
    flow_page_input: String,
    last_image_width: f32,
    last_image_height: f32,
    manhwa_mode: bool,
    manhwa_zoom: f32,
    manhwa_scroll_offset: f32,
    pending_manhwa_page_turn: Option<usize>,
    pending_resume_page: Option<usize>,
    awaiting_resume_page: Option<usize>,
    progress_store: ReadingProgressStore,
}

struct ManhwaPageLayout {
    texture_id: egui::TextureId,
    top: f32,
    size: egui::Vec2,
}

struct ManhwaScrollBounds {
    min: f32,
    max: f32,
    total_height: f32,
}

impl ComicReaderApp {
    pub fn new(
        creation_context: &eframe::CreationContext<'_>,
        initial_path: Option<PathBuf>,
    ) -> Self {
        install_system_fonts(&creation_context.egui_ctx);

        let mut app = Self {
            status: "按 O 打开 CBZ/ZIP 漫画".to_string(),
            manhwa_zoom: 1.0,
            ..Default::default()
        };

        if let Some(path) = initial_path {
            app.open_path(path);
        }

        app
    }

    fn open_path(&mut self, path: PathBuf) {
        let progress_key = progress_key_for_path(&path);
        let resume_page = self.progress_store.page_for_key(&progress_key);

        self.textures.clear();
        self.decoded_images.clear();
        self.image_sizes.clear();
        self.thumbnails.clear();
        self.last_thumbnail_request = None;
        self.current_page = 0;
        self.page_count = 0;
        self.flow_center = 0;
        self.flow_target = 0;
        self.flow_animated = 0.0;
        self.flow_page_input.clear();
        self.last_image_width = 0.0;
        self.last_image_height = 0.0;
        self.manhwa_zoom = 1.0;
        self.manhwa_scroll_offset = 0.0;
        self.pending_manhwa_page_turn = None;
        self.cache_status.clear();
        self.pending_resume_page = resume_page;
        self.awaiting_resume_page = None;
        self.status = format!("正在打开 {}", path.display());

        match backend_for_path(&path) {
            Ok(backend) => {
                let decoder = Arc::new(ImageCrateDecoder::new());
                self.handle = Some(spawn_reader(backend, decoder, ReaderOptions::default()));
                self.current_path = Some(path);
                self.current_progress_key = Some(progress_key);
            }
            Err(error) => {
                self.handle = None;
                self.current_path = None;
                self.current_progress_key = None;
                self.pending_resume_page = None;
                self.status = error.to_string();
            }
        }
    }

    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("打开漫画")
            .add_filter("Comic archives", &["cbz", "zip"])
            .pick_file()
        {
            self.open_path(path);
        }
    }

    fn save_current_page_dialog(&mut self) {
        let Some(image) = self.decoded_images.get(&self.current_page) else {
            self.status = "当前页还未加载完成，无法保存".to_string();
            return;
        };

        let default_name = format!("page-{:04}.png", self.current_page + 1);
        let Some(path) = rfd::FileDialog::new()
            .set_title("保存当前页")
            .add_filter("PNG image", &["png"])
            .set_file_name(default_name)
            .save_file()
        else {
            return;
        };

        let path = ensure_png_extension(path);
        match image::save_buffer_with_format(
            &path,
            &image.rgba,
            image.width,
            image.height,
            image::ColorType::Rgba8,
            image::ImageFormat::Png,
        ) {
            Ok(()) => {
                self.status = format!("已保存当前页到 {}", path.display());
            }
            Err(error) => {
                self.status = format!("保存当前页失败: {error}");
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
                    self.flow_page_input = "1".to_string();
                    self.status = format!("共 {pages} 页");
                    self.resume_saved_page_if_needed(&handle);
                }
                ReaderEvent::CurrentPage { page_index } => {
                    self.current_page = page_index;
                    self.set_flow_center(page_index);
                    if self.pending_manhwa_page_turn == Some(page_index) {
                        self.pending_manhwa_page_turn = None;
                    } else {
                        self.manhwa_scroll_offset = 0.0;
                        self.pending_manhwa_page_turn = None;
                    }
                    self.status = format!("第 {} / {} 页", page_index + 1, self.page_count.max(1));
                    self.upload_nearby_textures(ctx);
                    self.save_progress_if_current_page_is_settled(page_index);
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
        if self.manhwa_mode {
            page_index >= self.current_page.saturating_sub(1)
                && page_index <= self.current_page + MANHWA_LOOKAHEAD_PAGES
        } else {
            self.current_page.abs_diff(page_index) <= 1
        }
    }

    fn texture_window(&self) -> impl Iterator<Item = usize> {
        let start = self.current_page.saturating_sub(1);
        let lookahead = if self.manhwa_mode {
            MANHWA_LOOKAHEAD_PAGES
        } else {
            1
        };
        let end = (self.current_page + lookahead).min(self.page_count.saturating_sub(1));
        start..=end
    }

    fn trim_page_textures(&mut self) {
        let current = self.current_page;
        if self.manhwa_mode {
            self.textures.retain(|page, _| {
                *page >= current.saturating_sub(1) && *page <= current + MANHWA_LOOKAHEAD_PAGES
            });
        } else {
            self.textures.retain(|page, _| current.abs_diff(*page) <= 1);
        }
    }

    fn request_visible_thumbnails(&mut self) {
        const THUMB_RADIUS: usize = 8;

        let Some(handle) = &self.handle else {
            return;
        };

        if self.page_count == 0 || !self.show_thumbnails {
            return;
        }

        let request = (self.flow_center, THUMB_RADIUS);
        if self.last_thumbnail_request == Some(request) {
            return;
        }

        handle.request_thumbnails(self.flow_center, THUMB_RADIUS);
        self.last_thumbnail_request = Some(request);
    }

    fn set_flow_center(&mut self, page: usize) {
        if self.page_count == 0 {
            return;
        }

        let page = page.min(self.page_count - 1);
        self.flow_center = page;
        self.flow_target = page;
        self.flow_animated = page as f32;
        self.flow_page_input = (page + 1).to_string();
        self.last_thumbnail_request = None;
    }

    fn set_flow_target(&mut self, page: usize) {
        if self.page_count == 0 {
            return;
        }

        let page = page.min(self.page_count - 1);
        self.flow_target = page;
        self.flow_center = page;
        self.flow_page_input = (page + 1).to_string();
        self.last_thumbnail_request = None;
    }

    fn resume_saved_page_if_needed(&mut self, handle: &ReaderHandle) {
        if self.page_count == 0 {
            self.pending_resume_page = None;
            self.awaiting_resume_page = None;
            return;
        }

        let Some(saved_page) = self.pending_resume_page.take() else {
            return;
        };

        let page = saved_page.min(self.page_count - 1);
        if page == 0 {
            return;
        }

        self.awaiting_resume_page = Some(page);
        self.status = format!("继续阅读第 {} / {} 页", page + 1, self.page_count);
        handle.go_to(page);
    }

    fn save_progress_if_current_page_is_settled(&mut self, page_index: usize) {
        if self
            .awaiting_resume_page
            .is_some_and(|resume| page_index != resume)
        {
            return;
        }

        self.awaiting_resume_page = None;

        let Some(key) = &self.current_progress_key else {
            return;
        };

        if let Err(error) = self.progress_store.save_page(key, page_index) {
            self.status = format!("阅读进度保存失败: {error}");
        }
    }

    fn update_flow_animation(&mut self, ctx: &egui::Context) {
        let target = self.flow_target as f32;
        let delta = target - self.flow_animated;
        if delta.abs() < 0.01 {
            self.flow_animated = target;
            return;
        }

        self.flow_animated += delta * 0.22;
        ctx.request_repaint();
    }

    fn submit_flow_page_input(&mut self) {
        let Ok(page) = self.flow_page_input.trim().parse::<usize>() else {
            self.flow_page_input = (self.flow_center + 1).to_string();
            return;
        };

        let page = page
            .saturating_sub(1)
            .min(self.page_count.saturating_sub(1));
        self.set_flow_target(page);
        if let Some(handle) = &self.handle {
            handle.go_to(page);
        }
    }

    fn update_thumbnail_visibility(&mut self, ctx: &egui::Context) {
        const HOT_ZONE_HEIGHT: f32 = 36.0;
        let hide_above_bottom = self.flow_height(ctx.content_rect().height()) + 44.0;

        let Some(pointer) = ctx.pointer_hover_pos() else {
            self.show_thumbnails = false;
            return;
        };

        let bottom = ctx.content_rect().bottom();
        if pointer.y >= bottom - HOT_ZONE_HEIGHT {
            self.show_thumbnails = true;
        } else if pointer.y < bottom - hide_above_bottom {
            self.show_thumbnails = false;
        }
    }

    fn flow_height(&self, available_height: f32) -> f32 {
        let preferred = if self.last_image_height > 0.0 {
            self.last_image_height * 0.40
        } else {
            available_height * 0.32
        };

        preferred.clamp(220.0, (available_height * 0.48).max(220.0))
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("打开漫画 (O)").clicked() {
                self.open_file_dialog();
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

    fn handle_keyboard_shortcuts(&mut self, ctx: &egui::Context) {
        if self.handle.is_none() || self.page_count == 0 || ctx.egui_wants_keyboard_input() {
            return;
        }

        if ctx.input(|input| input.key_pressed(egui::Key::H)) {
            self.manhwa_mode = !self.manhwa_mode;
            if self.manhwa_mode {
                self.manhwa_zoom = 1.0;
                self.manhwa_scroll_offset = 0.0;
                self.pending_manhwa_page_turn = None;
                self.upload_nearby_textures(ctx);
                self.request_manhwa_pages();
                self.status = "韩漫模式：上下方向键滚动，Cmd +/- 缩放，H 返回普通模式".to_string();
            } else {
                self.pending_manhwa_page_turn = None;
                self.status = format!("第 {} / {} 页", self.current_page + 1, self.page_count);
            }
            ctx.request_repaint();
            return;
        }

        if self.manhwa_mode {
            let zoom_in = ctx.input(|input| {
                input.modifiers.command
                    && (input.key_pressed(egui::Key::Plus) || input.key_pressed(egui::Key::Equals))
            });
            let zoom_out =
                ctx.input(|input| input.modifiers.command && input.key_pressed(egui::Key::Minus));
            let scroll_up = ctx.input(|input| input.key_down(egui::Key::ArrowUp));
            let scroll_down = ctx.input(|input| input.key_down(egui::Key::ArrowDown));

            if zoom_in {
                self.adjust_manhwa_zoom(MANHWA_ZOOM_STEP);
                self.request_manhwa_pages();
                ctx.request_repaint();
            }
            if zoom_out {
                self.adjust_manhwa_zoom(-MANHWA_ZOOM_STEP);
                self.request_manhwa_pages();
                ctx.request_repaint();
            }
            if scroll_up {
                self.manhwa_scroll_offset -= manhwa_keyboard_scroll_step(ctx);
                ctx.request_repaint();
            }
            if scroll_down {
                self.manhwa_scroll_offset += manhwa_keyboard_scroll_step(ctx);
                ctx.request_repaint();
            }

            if ctx.input(|input| input.key_pressed(egui::Key::S)) {
                self.save_current_page_dialog();
            }
            return;
        }

        let previous = ctx.input(|input| input.key_pressed(egui::Key::ArrowLeft));
        let next = ctx.input(|input| input.key_pressed(egui::Key::ArrowRight));
        let save = ctx.input(|input| input.key_pressed(egui::Key::S));

        if previous && self.current_page > 0 {
            if let Some(handle) = &self.handle {
                handle.previous();
            }
        } else if next && self.current_page + 1 < self.page_count {
            if let Some(handle) = &self.handle {
                handle.next();
            }
        } else if save {
            self.save_current_page_dialog();
        }
    }

    fn handle_open_shortcut(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }

        if ctx.input(|input| input.key_pressed(egui::Key::O)) {
            self.open_file_dialog();
        }
    }

    fn image_panel(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_size();

        if self.manhwa_mode {
            self.manhwa_image_panel(ui, available);
            return;
        }

        let Some(texture) = self.textures.get(&self.current_page) else {
            ui.centered_and_justified(|ui| {
                ui.label(&self.status);
            });
            return;
        };

        let image_size = self
            .image_sizes
            .get(&self.current_page)
            .map(|[width, height]| egui::vec2(*width as f32, *height as f32))
            .unwrap_or_else(|| texture.size_vec2());
        let texture_id = texture.id();

        let panel_response = ui.interact(
            ui.max_rect(),
            ui.id().with("image-panel-context-menu"),
            egui::Sense::click(),
        );
        page_actions_context_menu(panel_response, self);

        let fit = fit_size(image_size, available);
        self.last_image_width = fit.x;
        self.last_image_height = fit.y;

        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    let response =
                        ui.add(egui::Image::new((texture_id, fit)).sense(egui::Sense::click()));
                    page_actions_context_menu(response, self);
                });
            });
    }

    fn manhwa_image_panel(&mut self, ui: &mut egui::Ui, available: egui::Vec2) {
        if !self.textures.contains_key(&self.current_page) {
            ui.centered_and_justified(|ui| {
                ui.label(&self.status);
            });
            return;
        }

        let panel_size = egui::vec2(available.x.max(1.0), available.y.max(1.0));
        let (rect, response) = ui.allocate_exact_size(panel_size, egui::Sense::click());
        let hovered = response.hovered();
        page_actions_context_menu(response, self);

        let base_width = manhwa_target_width(rect.width());
        let scroll_delta = if hovered {
            ui.input(|input| input.smooth_scroll_delta().y) * -MANHWA_WHEEL_SCROLL_SENSITIVITY
        } else {
            0.0
        };

        if scroll_delta.abs() > 0.0 {
            self.manhwa_scroll_offset += scroll_delta;
            ui.ctx().request_repaint();
        }

        self.request_manhwa_pages();
        self.normalize_manhwa_scroll(ui.ctx(), base_width);
        self.request_manhwa_pages();

        let pages = self.manhwa_visible_pages(base_width);
        let bounds = self.manhwa_scroll_bounds(base_width, rect.height());
        self.manhwa_scroll_offset = self.manhwa_scroll_offset.clamp(bounds.min, bounds.max);
        self.last_image_width = base_width * self.manhwa_zoom;
        self.last_image_height = rect.height().min(bounds.total_height);

        let painter = ui.painter().with_clip_rect(rect);
        for page in pages {
            let image_rect = egui::Rect::from_min_size(
                egui::pos2(
                    rect.center().x - page.size.x * 0.5,
                    rect.top() + page.top - self.manhwa_scroll_offset,
                ),
                page.size,
            );
            painter.image(
                page.texture_id,
                image_rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }

    fn adjust_manhwa_zoom(&mut self, delta: f32) {
        let old_zoom = self.manhwa_zoom;
        let new_zoom = (self.manhwa_zoom + delta).clamp(MANHWA_MIN_ZOOM, MANHWA_MAX_ZOOM);
        if (new_zoom - old_zoom).abs() <= f32::EPSILON {
            return;
        }

        self.manhwa_zoom = new_zoom;
        self.manhwa_scroll_offset *= new_zoom / old_zoom;
    }

    fn normalize_manhwa_scroll(&mut self, ctx: &egui::Context, base_width: f32) {
        while self.manhwa_scroll_offset < 0.0 && self.current_page > 0 {
            let previous_page = self.current_page - 1;
            let Some(previous_height) = self
                .manhwa_display_size(previous_page, base_width)
                .map(|size| size.y)
            else {
                self.manhwa_scroll_offset = 0.0;
                return;
            };
            if !self.textures.contains_key(&previous_page) {
                self.manhwa_scroll_offset = 0.0;
                return;
            }

            self.current_page = previous_page;
            self.set_flow_center(previous_page);
            self.manhwa_scroll_offset += previous_height + MANHWA_PAGE_GAP;
            self.queue_manhwa_page_turn(previous_page);
            self.upload_nearby_textures(ctx);
        }

        while self.current_page + 1 < self.page_count {
            let Some(current_height) = self
                .manhwa_display_size(self.current_page, base_width)
                .map(|size| size.y)
            else {
                return;
            };
            let current_extent = current_height + MANHWA_PAGE_GAP;
            if self.manhwa_scroll_offset < current_extent {
                break;
            }
            if !self.textures.contains_key(&(self.current_page + 1)) {
                break;
            }

            let next_page = self.current_page + 1;
            self.current_page = next_page;
            self.set_flow_center(next_page);
            self.manhwa_scroll_offset -= current_extent;
            self.queue_manhwa_page_turn(next_page);
            self.upload_nearby_textures(ctx);
        }
    }

    fn request_manhwa_pages(&self) {
        let Some(handle) = &self.handle else {
            return;
        };
        if self.page_count == 0 {
            return;
        }

        let start = self.current_page.saturating_sub(1);
        let end = (self.current_page + MANHWA_LOOKAHEAD_PAGES).min(self.page_count - 1);
        handle.request_pages((start..=end).collect());
    }

    fn queue_manhwa_page_turn(&mut self, page: usize) {
        if self.pending_manhwa_page_turn == Some(page) {
            return;
        }

        self.pending_manhwa_page_turn = Some(page);
        if let Some(handle) = &self.handle {
            handle.go_to(page);
        }
    }

    fn manhwa_visible_pages(&self, base_width: f32) -> Vec<ManhwaPageLayout> {
        let mut pages = Vec::new();

        if self.current_page > 0 {
            let previous_page = self.current_page - 1;
            if let (Some(texture), Some(size)) = (
                self.textures.get(&previous_page),
                self.manhwa_display_size(previous_page, base_width),
            ) {
                pages.push(ManhwaPageLayout {
                    texture_id: texture.id(),
                    top: -(size.y + MANHWA_PAGE_GAP),
                    size,
                });
            }
        }

        let mut top = 0.0;
        for page in self.current_page
            ..self
                .page_count
                .min(self.current_page + MANHWA_LOOKAHEAD_PAGES + 1)
        {
            let Some(texture) = self.textures.get(&page) else {
                top += self.estimated_manhwa_display_height(base_width) + MANHWA_PAGE_GAP;
                continue;
            };
            let Some(size) = self.manhwa_display_size(page, base_width) else {
                top += self.estimated_manhwa_display_height(base_width) + MANHWA_PAGE_GAP;
                continue;
            };
            pages.push(ManhwaPageLayout {
                texture_id: texture.id(),
                top,
                size,
            });
            top += size.y + MANHWA_PAGE_GAP;
        }

        pages
    }

    fn manhwa_scroll_bounds(&self, base_width: f32, viewport_height: f32) -> ManhwaScrollBounds {
        let previous_extent = if self.current_page > 0 {
            self.manhwa_display_size(self.current_page - 1, base_width)
                .map(|size| size.y)
                .unwrap_or_else(|| self.estimated_manhwa_display_height(base_width))
                + MANHWA_PAGE_GAP
        } else {
            0.0
        };

        let mut total_height = 0.0;
        for page in self.current_page
            ..self
                .page_count
                .min(self.current_page + MANHWA_LOOKAHEAD_PAGES + 1)
        {
            let height = self
                .manhwa_display_size(page, base_width)
                .map(|size| size.y)
                .unwrap_or_else(|| self.estimated_manhwa_display_height(base_width));
            total_height += height;
            if page + 1 < self.page_count {
                total_height += MANHWA_PAGE_GAP;
            }
        }

        ManhwaScrollBounds {
            min: -previous_extent,
            max: (total_height - viewport_height).max(0.0),
            total_height,
        }
    }

    fn estimated_manhwa_display_height(&self, base_width: f32) -> f32 {
        self.manhwa_display_size(self.current_page, base_width)
            .map(|size| size.y)
            .unwrap_or(base_width * self.manhwa_zoom * 1.5)
    }

    fn manhwa_display_size(&self, page: usize, base_width: f32) -> Option<egui::Vec2> {
        let image_size = self
            .image_sizes
            .get(&page)
            .map(|[width, height]| egui::vec2(*width as f32, *height as f32))
            .or_else(|| self.textures.get(&page).map(|texture| texture.size_vec2()))?;
        if image_size.x <= 0.0 || image_size.y <= 0.0 {
            return None;
        }

        let width = base_width * self.manhwa_zoom;
        Some(egui::vec2(width, image_size.y * (width / image_size.x)))
    }

    fn thumbnail_strip(&mut self, ui: &mut egui::Ui) {
        if self.page_count == 0 {
            return;
        }

        const VISIBLE_RADIUS: isize = 5;
        const SIDE_THUMB_SCALE: f32 = 0.95;
        const CENTER_SIDE_GAP_RATIO: f32 = 0.08;
        const SIDE_STACK_STEP_RATIO: f32 = 0.35;

        self.update_flow_animation(ui.ctx());

        let available_width = ui.available_width();
        let height = ui.available_height().max(220.0);
        let center_thumb_height = height * 0.72;
        let center_thumb_width = center_thumb_height * 0.62;
        let side_thumb_width = center_thumb_width * SIDE_THUMB_SCALE;
        let center_side_step = (center_thumb_width + side_thumb_width) * 0.5
            + center_thumb_width * CENTER_SIDE_GAP_RATIO;
        let side_stack_step = side_thumb_width * SIDE_STACK_STEP_RATIO;
        let strip_width = if self.last_image_width > 0.0 {
            self.last_image_width.min(available_width)
        } else {
            available_width
        };
        let (outer_rect, _) =
            ui.allocate_exact_size(egui::vec2(available_width, height), egui::Sense::hover());
        let rect = egui::Rect::from_center_size(
            outer_rect.center(),
            egui::vec2(strip_width.max(360.0).min(available_width), height),
        );

        let response = ui.interact(
            rect,
            ui.id().with("thumbnail-flow"),
            egui::Sense::click_and_drag(),
        );
        if response.hovered() && !self.manhwa_mode {
            let scroll = ui.input(|input| input.smooth_scroll_delta().y);
            if scroll.abs() > 0.0 {
                let direction = if scroll < 0.0 { 1 } else { -1 };
                let page = self
                    .flow_target
                    .saturating_add_signed(direction)
                    .min(self.page_count - 1);
                self.set_flow_target(page);
                ui.ctx().request_repaint();
            }
        }

        let painter = ui.painter_at(rect);
        painter.rect_filled(
            rect,
            egui::CornerRadius::same(16),
            egui::Color32::from_rgba_premultiplied(18, 18, 22, 232),
        );
        painter.rect_stroke(
            rect,
            egui::CornerRadius::same(16),
            egui::Stroke::new(1.0, egui::Color32::from_gray(72)),
            egui::StrokeKind::Outside,
        );

        let page_text = format!("{} / {}", self.current_page + 1, self.page_count);
        painter.text(
            rect.left_top() + egui::vec2(16.0, 13.0),
            egui::Align2::LEFT_TOP,
            page_text,
            egui::FontId::proportional(17.0),
            egui::Color32::WHITE,
        );

        let center_x = rect.center().x;
        let base_y = rect.top() + height * 0.48;
        let start = (self.flow_center as isize - VISIBLE_RADIUS).max(0) as usize;
        let end = (self.flow_center + VISIBLE_RADIUS as usize).min(self.page_count - 1);

        let mut pages: Vec<usize> = (start..=end).collect();
        pages.sort_by(|a, b| {
            let a_distance = (*a as f32 - self.flow_animated).abs();
            let b_distance = (*b as f32 - self.flow_animated).abs();
            b_distance.total_cmp(&a_distance)
        });

        for page in pages {
            let offset = page as f32 - self.flow_animated;
            if offset.abs() > VISIBLE_RADIUS as f32 + 0.5 {
                continue;
            }

            let abs_offset = offset.abs();
            let depth = (1.0 - abs_offset * 0.12).clamp(0.28, 1.0);
            let selected = page == self.flow_target;
            let max_size = egui::vec2(center_thumb_width, center_thumb_height);
            let size = if selected {
                max_size
            } else {
                max_size * SIDE_THUMB_SCALE
            };
            let fold = offset.signum() * abs_offset.min(1.0) * center_thumb_width * 0.20;
            let x_distance = if abs_offset <= 1.0 {
                center_side_step * abs_offset
            } else {
                center_side_step + (abs_offset - 1.0) * side_stack_step
            };
            let y_offset = abs_offset.min(1.0) * center_thumb_height * 0.08;
            let center = egui::pos2(center_x + offset.signum() * x_distance, base_y + y_offset);
            let thumb_rect = egui::Rect::from_center_size(center, size);
            let shadow = thumb_rect.translate(egui::vec2(fold * 0.18, 6.0));

            painter.rect_filled(
                shadow,
                egui::CornerRadius::same(7),
                egui::Color32::from_rgba_premultiplied(0, 0, 0, 72),
            );

            let page_response = ui.interact(
                thumb_rect.expand(8.0),
                ui.id().with(("flow-page", page)),
                egui::Sense::click(),
            );
            if page_response.clicked() {
                self.set_flow_target(page);
            }
            if page_response.double_clicked() {
                self.set_flow_target(page);
                if let Some(handle) = &self.handle {
                    handle.go_to(page);
                }
            }

            if let Some(texture) = self.thumbnails.get(&page) {
                let image_rect = fit_rect(texture.size_vec2(), thumb_rect);
                let image_quad = flow_thumb_quad(image_rect, offset);
                paint_textured_quad(
                    &painter,
                    texture.id(),
                    image_quad,
                    egui::Color32::WHITE.linear_multiply(depth),
                );

                let edge_width = (fold.abs() * 0.45).max(2.0);
                let edge_rect = if offset < 0.0 {
                    egui::Rect::from_min_max(
                        image_rect.right_top() - egui::vec2(edge_width, 0.0),
                        image_rect.right_bottom(),
                    )
                } else {
                    egui::Rect::from_min_max(
                        image_rect.left_top(),
                        image_rect.left_bottom() + egui::vec2(edge_width, 0.0),
                    )
                };
                painter.rect_filled(
                    edge_rect,
                    egui::CornerRadius::same(2),
                    egui::Color32::from_rgba_premultiplied(
                        0,
                        0,
                        0,
                        (70.0 * offset.abs().min(1.0)) as u8,
                    ),
                );
            } else {
                painter.text(
                    thumb_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("{}", page + 1),
                    egui::FontId::proportional(20.0),
                    egui::Color32::LIGHT_GRAY,
                );
            }

            painter.text(
                egui::pos2(thumb_rect.center().x, rect.bottom() - 22.0),
                egui::Align2::CENTER_CENTER,
                format!("{}", page + 1),
                egui::FontId::proportional(if selected { 15.0 } else { 13.0 }),
                if selected {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::GRAY
                },
            );
        }

        ui.scope_builder(
            egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
                rect.right_top() + egui::vec2(-132.0, 10.0),
                egui::vec2(116.0, 28.0),
            )),
            |ui| {
                ui.horizontal(|ui| {
                    let response = ui.add_sized(
                        [62.0, 24.0],
                        egui::TextEdit::singleline(&mut self.flow_page_input)
                            .horizontal_align(egui::Align::Center),
                    );
                    let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if response.lost_focus() && enter {
                        self.submit_flow_page_input();
                    }
                    if ui.button("跳转").clicked() {
                        self.submit_flow_page_input();
                    }
                });
            },
        );
    }
}

impl eframe::App for ComicReaderApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_events(&ctx);
        self.handle_open_shortcut(&ctx);
        self.handle_keyboard_shortcuts(&ctx);
        self.update_thumbnail_visibility(&ctx);
        self.request_visible_thumbnails();

        ui.vertical(|ui| {
            let reading_mode = self.handle.is_some();
            if !reading_mode {
                self.top_bar(ui);
                ui.separator();
            }

            let image_height = if reading_mode {
                ui.available_height()
            } else {
                (ui.available_height() - 28.0).max(0.0)
            };
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), image_height),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    self.image_panel(ui);
                },
            );

            if self.show_thumbnails && self.page_count > 0 {
                let rect = ui.max_rect();
                let flow_height = self.flow_height(rect.height());
                egui::Area::new(egui::Id::new("thumbnail-flow-area"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(egui::pos2(rect.left(), rect.bottom() - flow_height - 8.0))
                    .show(&ctx, |ui| {
                        ui.set_width(rect.width());
                        ui.set_height(flow_height);
                        self.thumbnail_strip(ui);
                    });
            }

            if !reading_mode {
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
            }
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

fn page_actions_context_menu(response: egui::Response, app: &mut ComicReaderApp) {
    response.context_menu(|ui| {
        if ui.button("Save").clicked() {
            app.save_current_page_dialog();
            ui.close();
        }
    });
}

fn ensure_png_extension(mut path: PathBuf) -> PathBuf {
    if path.extension().is_none() {
        path.set_extension("png");
    }

    path
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

#[derive(Debug, Clone)]
struct ReadingProgressStore {
    path: PathBuf,
    pages_by_key: HashMap<String, usize>,
}

impl Default for ReadingProgressStore {
    fn default() -> Self {
        Self::load(default_reading_progress_path())
    }
}

impl ReadingProgressStore {
    fn load(path: PathBuf) -> Self {
        let mut pages_by_key = fs::read_to_string(&path)
            .ok()
            .map(|contents| parse_progress_entries(&contents))
            .unwrap_or_default();

        if pages_by_key.is_empty() {
            let legacy_path = PathBuf::from(LEGACY_READING_PROGRESS_PATH);
            pages_by_key = fs::read_to_string(&legacy_path)
                .ok()
                .map(|contents| parse_progress_entries(&contents))
                .unwrap_or_default();
        }

        Self { path, pages_by_key }
    }

    fn page_for_key(&self, key: &str) -> Option<usize> {
        self.pages_by_key.get(key).copied()
    }

    fn save_page(&mut self, key: &str, page: usize) -> std::io::Result<()> {
        self.pages_by_key.insert(key.to_string(), page);
        self.flush()
    }

    fn flush(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut entries: Vec<_> = self.pages_by_key.iter().collect();
        entries.sort_by_key(|(key, _)| *key);

        let mut contents =
            String::from("# RustComicReader reading progress\n# path\tzero_based_page\n");
        for (key, page) in entries {
            contents.push_str(&escape_progress_key(key));
            contents.push('\t');
            contents.push_str(&page.to_string());
            contents.push('\n');
        }

        fs::write(&self.path, contents)
    }
}

fn default_reading_progress_path() -> PathBuf {
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("RustComicReader")
            .join("reading_progress.tsv");
    }

    PathBuf::from(LEGACY_READING_PROGRESS_PATH)
}

fn progress_key_for_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn parse_progress_entries(contents: &str) -> HashMap<String, usize> {
    let mut entries = HashMap::new();

    for line in contents.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, page)) = line.split_once('\t') else {
            continue;
        };
        let Ok(page) = page.parse::<usize>() else {
            continue;
        };

        entries.insert(unescape_progress_key(key), page);
    }

    entries
}

fn escape_progress_key(key: &str) -> String {
    key.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

fn unescape_progress_key(key: &str) -> String {
    let mut output = String::with_capacity(key.len());
    let mut chars = key.chars();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }

        match chars.next() {
            Some('\\') => output.push('\\'),
            Some('t') => output.push('\t'),
            Some('n') => output.push('\n'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }

    output
}

fn fit_size(image_size: egui::Vec2, available: egui::Vec2) -> egui::Vec2 {
    if image_size.x <= 0.0 || image_size.y <= 0.0 || available.x <= 0.0 || available.y <= 0.0 {
        return image_size;
    }

    let scale = (available.x / image_size.x).min(available.y / image_size.y);
    image_size * scale.min(1.0)
}

fn manhwa_target_width(available_width: f32) -> f32 {
    if available_width <= 0.0 {
        return 0.0;
    }

    let ratio = if available_width < 900.0 {
        MANHWA_NARROW_WIDTH_RATIO
    } else {
        MANHWA_DEFAULT_WIDTH_RATIO
    };
    (available_width * ratio).min(MANHWA_MAX_CONTENT_WIDTH)
}

fn manhwa_keyboard_scroll_step(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().height() * 0.08).clamp(36.0, 96.0)
}

fn fit_rect(image_size: egui::Vec2, bounds: egui::Rect) -> egui::Rect {
    if image_size.x <= 0.0 || image_size.y <= 0.0 || bounds.width() <= 0.0 || bounds.height() <= 0.0
    {
        return bounds;
    }

    let scale = (bounds.width() / image_size.x).min(bounds.height() / image_size.y);
    let size = image_size * scale;
    egui::Rect::from_center_size(bounds.center(), size)
}

fn flow_thumb_quad(rect: egui::Rect, offset: f32) -> [egui::Pos2; 4] {
    if offset.abs() < 0.35 {
        return [
            rect.left_top(),
            rect.right_top(),
            rect.right_bottom(),
            rect.left_bottom(),
        ];
    }

    let fold = offset.abs().min(3.0) / 3.0;
    let inset = rect.height() * (0.10 + 0.01 * fold);
    if offset < 0.0 {
        [
            rect.left_top(),
            rect.right_top() + egui::vec2(0.0, inset),
            rect.right_bottom() - egui::vec2(0.0, inset),
            rect.left_bottom(),
        ]
    } else {
        [
            rect.left_top() + egui::vec2(0.0, inset),
            rect.right_top(),
            rect.right_bottom(),
            rect.left_bottom() - egui::vec2(0.0, inset),
        ]
    }
}

fn paint_textured_quad(
    painter: &egui::Painter,
    texture_id: egui::TextureId,
    points: [egui::Pos2; 4],
    tint: egui::Color32,
) {
    let mut mesh = egui::Mesh::with_texture(texture_id);
    mesh.vertices.extend_from_slice(&[
        egui::epaint::Vertex {
            pos: points[0],
            uv: egui::pos2(0.0, 0.0),
            color: tint,
        },
        egui::epaint::Vertex {
            pos: points[1],
            uv: egui::pos2(1.0, 0.0),
            color: tint,
        },
        egui::epaint::Vertex {
            pos: points[2],
            uv: egui::pos2(1.0, 1.0),
            color: tint,
        },
        egui::epaint::Vertex {
            pos: points[3],
            uv: egui::pos2(0.0, 1.0),
            color: tint,
        },
    ]);
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn progress_entries_round_trip_escaped_paths() {
        let key = "/tmp/comics/tab\tand\\newline\ncomic.cbz";
        let contents = format!("{}\t42\n", escape_progress_key(key));

        let entries = parse_progress_entries(&contents);

        assert_eq!(entries.get(key), Some(&42));
    }

    #[test]
    fn save_path_defaults_to_png_extension() {
        assert_eq!(
            ensure_png_extension(PathBuf::from("/tmp/page-0001")),
            PathBuf::from("/tmp/page-0001.png")
        );
        assert_eq!(
            ensure_png_extension(PathBuf::from("/tmp/page-0001.png")),
            PathBuf::from("/tmp/page-0001.png")
        );
    }

    #[test]
    fn progress_store_flushes_loaded_entries_to_configured_path() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        let path = env::temp_dir()
            .join("rustcomicreader-tests")
            .join(format!("reading-progress-{unique}.tsv"));

        let mut store = ReadingProgressStore::load(path.clone());
        store
            .save_page("/tmp/comic.cbz", 7)
            .expect("progress should save to configured path");

        let contents = fs::read_to_string(&path).expect("progress file should exist");
        assert!(contents.contains("/tmp/comic.cbz\t7"));
    }
}
