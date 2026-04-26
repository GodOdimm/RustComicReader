use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use crossbeam_channel::{unbounded, Receiver, Sender, TryRecvError};
use thiserror::Error;

pub type PageIndex = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(pub usize);

#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    pub id: EntryId,
    pub name: String,
    pub uncompressed_size: u64,
}

#[derive(Debug, Clone)]
pub struct PageDescriptor {
    pub page_index: PageIndex,
    pub entry_id: EntryId,
    pub name: String,
    pub physical_index: usize,
}

#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl DecodedImage {
    pub fn byte_len(&self) -> usize {
        self.rgba.len()
    }
}

#[derive(Debug, Error)]
pub enum ReaderError {
    #[error("archive error: {0}")]
    Archive(String),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("comic contains no supported image pages")]
    EmptyComic,
    #[error("page index {index} is outside page count {page_count}")]
    PageOutOfRange { index: PageIndex, page_count: usize },
}

pub type Result<T> = std::result::Result<T, ReaderError>;

pub trait ArchiveBackend: Send {
    fn list_entries(&mut self) -> Result<Vec<ArchiveEntry>>;
    fn read_entry(&mut self, entry_id: EntryId) -> Result<Bytes>;
}

pub trait ImageDecoder: Send + Sync + 'static {
    fn decode(&self, bytes: &[u8]) -> Result<DecodedImage>;
    fn thumbnail(&self, bytes: &[u8], max_edge: u32) -> Result<DecodedImage>;
    fn thumbnail_from_decoded(&self, image: &DecodedImage, max_edge: u32) -> Result<DecodedImage>;
}

pub trait Cache<V: Clone + Send> {
    fn get(&mut self, page: PageIndex) -> Option<V>;
    fn insert(&mut self, page: PageIndex, value: V, byte_len: usize);
    fn contains(&self, page: PageIndex) -> bool;
    fn clear(&mut self);
    fn bytes_used(&self) -> usize;
}

#[derive(Debug, Clone)]
pub struct PageOrder {
    pages: Vec<PageDescriptor>,
}

impl PageOrder {
    pub fn from_entries(entries: Vec<ArchiveEntry>) -> Self {
        let mut pages: Vec<_> = entries
            .into_iter()
            .enumerate()
            .filter(|(_, entry)| is_supported_image_name(&entry.name))
            .map(|(physical_index, entry)| PageDescriptor {
                page_index: 0,
                entry_id: entry.id,
                name: entry.name,
                physical_index,
            })
            .collect();

        pages.sort_by(|a, b| natord::compare(&a.name, &b.name));

        for (page_index, page) in pages.iter_mut().enumerate() {
            page.page_index = page_index;
        }

        Self { pages }
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    pub fn page(&self, index: PageIndex) -> Option<&PageDescriptor> {
        self.pages.get(index)
    }

    pub fn pages(&self) -> &[PageDescriptor] {
        &self.pages
    }

    pub fn prioritized_window(
        &self,
        current: PageIndex,
        forward: usize,
        backward: usize,
    ) -> Vec<PageIndex> {
        let mut pages = Vec::with_capacity(forward + backward + 1);
        if current >= self.len() {
            return pages;
        }

        pages.push(current);

        for offset in 1..=forward.max(backward) {
            if offset <= forward {
                let next = current + offset;
                if next < self.len() {
                    pages.push(next);
                }
            }

            if offset <= backward && current >= offset {
                pages.push(current - offset);
            }
        }

        pages
    }
}

pub fn is_supported_image_name(name: &str) -> bool {
    matches!(
        Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "tif" | "tiff")
    )
}

#[derive(Debug)]
struct CacheItem<V> {
    value: V,
    byte_len: usize,
}

#[derive(Debug)]
pub struct LruPageCache<V> {
    max_bytes: usize,
    bytes_used: usize,
    items: HashMap<PageIndex, CacheItem<V>>,
    order: VecDeque<PageIndex>,
}

impl<V> LruPageCache<V> {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            bytes_used: 0,
            items: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    fn touch(&mut self, page: PageIndex) {
        self.order.retain(|candidate| *candidate != page);
        self.order.push_back(page);
    }

    fn evict_until_fit(&mut self) {
        while self.bytes_used > self.max_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };

            if let Some(removed) = self.items.remove(&oldest) {
                self.bytes_used = self.bytes_used.saturating_sub(removed.byte_len);
            }
        }
    }
}

impl<V: Clone + Send> Cache<V> for LruPageCache<V> {
    fn get(&mut self, page: PageIndex) -> Option<V> {
        let value = self.items.get(&page).map(|item| item.value.clone());
        if value.is_some() {
            self.touch(page);
        }
        value
    }

    fn insert(&mut self, page: PageIndex, value: V, byte_len: usize) {
        if let Some(existing) = self.items.remove(&page) {
            self.bytes_used = self.bytes_used.saturating_sub(existing.byte_len);
        }

        self.items.insert(page, CacheItem { value, byte_len });
        self.bytes_used += byte_len;
        self.touch(page);
        self.evict_until_fit();
    }

    fn contains(&self, page: PageIndex) -> bool {
        self.items.contains_key(&page)
    }

    fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.bytes_used = 0;
    }

    fn bytes_used(&self) -> usize {
        self.bytes_used
    }
}

pub type RawPageCache = LruPageCache<Bytes>;
pub type DisplayCache = LruPageCache<DecodedImage>;
pub type ThumbnailCache = LruPageCache<DecodedImage>;

#[derive(Debug, Clone)]
pub struct ReaderOptions {
    pub raw_cache_bytes: usize,
    pub display_cache_bytes: usize,
    pub thumbnail_cache_bytes: usize,
    pub prefetch_forward: usize,
    pub prefetch_backward: usize,
    pub thumbnail_edge: u32,
}

impl Default for ReaderOptions {
    fn default() -> Self {
        Self {
            raw_cache_bytes: 256 * 1024 * 1024,
            display_cache_bytes: 512 * 1024 * 1024,
            thumbnail_cache_bytes: 64 * 1024 * 1024,
            prefetch_forward: 4,
            prefetch_backward: 4,
            thumbnail_edge: 256,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ReaderCommand {
    GoTo(PageIndex),
    Next,
    Previous,
    RequestThumbnails { center: PageIndex, radius: usize },
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum ReaderEvent {
    PageCount {
        pages: usize,
    },
    CurrentPage {
        page_index: PageIndex,
    },
    LoadingPage {
        page_index: PageIndex,
    },
    PageDecoded {
        page_index: PageIndex,
        image: Arc<DecodedImage>,
        from_cache: bool,
        elapsed_ms: u128,
    },
    ThumbnailDecoded {
        page_index: PageIndex,
        image: Arc<DecodedImage>,
    },
    CacheStats {
        raw_bytes: usize,
        display_bytes: usize,
        thumbnail_bytes: usize,
    },
    Finished,
    Error(String),
}

#[derive(Clone)]
pub struct ReaderHandle {
    command_tx: Sender<ReaderCommand>,
    event_rx: Receiver<ReaderEvent>,
}

impl ReaderHandle {
    pub fn go_to(&self, page: PageIndex) {
        let _ = self.command_tx.send(ReaderCommand::GoTo(page));
    }

    pub fn next(&self) {
        let _ = self.command_tx.send(ReaderCommand::Next);
    }

    pub fn previous(&self) {
        let _ = self.command_tx.send(ReaderCommand::Previous);
    }

    pub fn request_thumbnails(&self, center: PageIndex, radius: usize) {
        let _ = self
            .command_tx
            .send(ReaderCommand::RequestThumbnails { center, radius });
    }

    pub fn shutdown(&self) {
        let _ = self.command_tx.send(ReaderCommand::Shutdown);
    }

    pub fn try_recv(&self) -> Option<ReaderEvent> {
        self.event_rx.try_recv().ok()
    }

    pub fn recv(&self) -> Option<ReaderEvent> {
        self.event_rx.recv().ok()
    }
}

pub fn spawn_reader(
    backend: Box<dyn ArchiveBackend>,
    decoder: Arc<dyn ImageDecoder>,
    options: ReaderOptions,
) -> ReaderHandle {
    let (command_tx, command_rx) = unbounded();
    let (event_tx, event_rx) = unbounded();

    thread::spawn(move || {
        let mut worker = ReaderWorker::new(backend, decoder, options, command_rx, event_tx);
        worker.run();
    });

    ReaderHandle {
        command_tx,
        event_rx,
    }
}

struct ReaderWorker {
    backend: Box<dyn ArchiveBackend>,
    decoder: Arc<dyn ImageDecoder>,
    options: ReaderOptions,
    command_rx: Receiver<ReaderCommand>,
    event_tx: Sender<ReaderEvent>,
    raw_cache: RawPageCache,
    display_cache: DisplayCache,
    thumbnail_cache: ThumbnailCache,
    prefetch_queue: VecDeque<PageIndex>,
    thumbnail_queue: VecDeque<PageIndex>,
    queued_thumbnails: HashSet<PageIndex>,
    prefetches_since_thumbnail: usize,
}

impl ReaderWorker {
    fn new(
        backend: Box<dyn ArchiveBackend>,
        decoder: Arc<dyn ImageDecoder>,
        options: ReaderOptions,
        command_rx: Receiver<ReaderCommand>,
        event_tx: Sender<ReaderEvent>,
    ) -> Self {
        Self {
            raw_cache: RawPageCache::new(options.raw_cache_bytes),
            display_cache: DisplayCache::new(options.display_cache_bytes),
            thumbnail_cache: ThumbnailCache::new(options.thumbnail_cache_bytes),
            backend,
            decoder,
            options,
            command_rx,
            event_tx,
            prefetch_queue: VecDeque::new(),
            thumbnail_queue: VecDeque::new(),
            queued_thumbnails: HashSet::new(),
            prefetches_since_thumbnail: 0,
        }
    }

    fn run(&mut self) {
        let entries = match self.backend.list_entries() {
            Ok(entries) => entries,
            Err(error) => {
                self.emit_error(error);
                return;
            }
        };

        let order = PageOrder::from_entries(entries);
        if order.is_empty() {
            self.emit_error(ReaderError::EmptyComic);
            return;
        }

        let mut current = 0;
        let page_count = order.len();
        let _ = self
            .event_tx
            .send(ReaderEvent::PageCount { pages: page_count });
        let _ = self.event_tx.send(ReaderEvent::CurrentPage {
            page_index: current,
        });
        self.rebuild_prefetch_queue(&order, current);

        loop {
            if let Some(control) = self.drain_commands(current, page_count) {
                match control {
                    WindowResult::JumpTo(next) => {
                        current = next;
                        let _ = self.event_tx.send(ReaderEvent::CurrentPage {
                            page_index: current,
                        });
                        self.rebuild_prefetch_queue(&order, current);
                        continue;
                    }
                    WindowResult::Shutdown => break,
                    WindowResult::Continue => {}
                }
            }

            if let Err(error) = self.ensure_decoded(&order, current) {
                self.emit_error(error);
            }
            self.emit_cache_stats();

            if let Some(control) = self.wait_for_command_if_idle(current, page_count) {
                match control {
                    WindowResult::JumpTo(next) => {
                        current = next;
                        let _ = self.event_tx.send(ReaderEvent::CurrentPage {
                            page_index: current,
                        });
                        self.rebuild_prefetch_queue(&order, current);
                    }
                    WindowResult::Shutdown => break,
                    WindowResult::Continue => {}
                }
                continue;
            }

            if self.should_run_thumbnail_before_prefetch() {
                if let Some(page) = self.thumbnail_queue.pop_front() {
                    self.queued_thumbnails.remove(&page);
                    if let Err(error) = self.ensure_thumbnail(&order, page) {
                        self.emit_error(error);
                    }
                    self.prefetches_since_thumbnail = 0;
                    self.emit_cache_stats();
                    continue;
                }
            }

            if let Some(page) = self.prefetch_queue.pop_front() {
                if page != current && !self.display_cache.contains(page) {
                    if let Err(error) = self.ensure_decoded(&order, page) {
                        self.emit_error(error);
                    }
                    self.prefetches_since_thumbnail += 1;
                    self.emit_cache_stats();
                }
                continue;
            }

            if let Some(page) = self.thumbnail_queue.pop_front() {
                self.queued_thumbnails.remove(&page);
                if let Err(error) = self.ensure_thumbnail(&order, page) {
                    self.emit_error(error);
                }
                self.emit_cache_stats();
                continue;
            }
        }

        let _ = self.event_tx.send(ReaderEvent::Finished);
    }

    fn rebuild_prefetch_queue(&mut self, order: &PageOrder, current: PageIndex) {
        let pages = order.prioritized_window(
            current,
            self.options.prefetch_forward,
            self.options.prefetch_backward,
        );

        self.prefetch_queue = pages
            .into_iter()
            .filter(|page| *page != current)
            .collect::<VecDeque<_>>();
        self.prefetches_since_thumbnail = 0;
    }

    fn should_run_thumbnail_before_prefetch(&self) -> bool {
        !self.thumbnail_queue.is_empty()
            && !self.prefetch_queue.is_empty()
            && self.prefetches_since_thumbnail >= 2
    }

    fn ensure_decoded(&mut self, order: &PageOrder, page: PageIndex) -> Result<()> {
        if let Some(image) = self.display_cache.get(page) {
            let _ = self.event_tx.send(ReaderEvent::PageDecoded {
                page_index: page,
                image: Arc::new(image),
                from_cache: true,
                elapsed_ms: 0,
            });
            return Ok(());
        }

        let page_descriptor = order.page(page).ok_or(ReaderError::PageOutOfRange {
            index: page,
            page_count: order.len(),
        })?;

        let _ = self
            .event_tx
            .send(ReaderEvent::LoadingPage { page_index: page });

        let start = Instant::now();
        let raw = match self.raw_cache.get(page) {
            Some(raw) => raw,
            None => {
                let raw = self.backend.read_entry(page_descriptor.entry_id)?;
                self.raw_cache.insert(page, raw.clone(), raw.len());
                raw
            }
        };

        let decoded = self.decoder.decode(&raw)?;
        let elapsed_ms = start.elapsed().as_millis();
        self.display_cache
            .insert(page, decoded.clone(), decoded.byte_len());

        let _ = self.event_tx.send(ReaderEvent::PageDecoded {
            page_index: page,
            image: Arc::new(decoded),
            from_cache: false,
            elapsed_ms,
        });

        Ok(())
    }

    fn ensure_thumbnail(&mut self, order: &PageOrder, page: PageIndex) -> Result<()> {
        if let Some(thumbnail) = self.thumbnail_cache.get(page) {
            let _ = self.event_tx.send(ReaderEvent::ThumbnailDecoded {
                page_index: page,
                image: Arc::new(thumbnail),
            });
            return Ok(());
        }

        let thumbnail = if let Some(decoded) = self.display_cache.get(page) {
            self.decoder
                .thumbnail_from_decoded(&decoded, self.options.thumbnail_edge)?
        } else {
            let page_descriptor = order.page(page).ok_or(ReaderError::PageOutOfRange {
                index: page,
                page_count: order.len(),
            })?;
            let raw = match self.raw_cache.get(page) {
                Some(raw) => raw,
                None => {
                    let raw = self.backend.read_entry(page_descriptor.entry_id)?;
                    self.raw_cache.insert(page, raw.clone(), raw.len());
                    raw
                }
            };
            self.decoder.thumbnail(&raw, self.options.thumbnail_edge)?
        };

        self.thumbnail_cache
            .insert(page, thumbnail.clone(), thumbnail.byte_len());
        let _ = self.event_tx.send(ReaderEvent::ThumbnailDecoded {
            page_index: page,
            image: Arc::new(thumbnail),
        });
        Ok(())
    }

    fn drain_commands(&mut self, current: PageIndex, page_count: usize) -> Option<WindowResult> {
        let mut latest = None;

        loop {
            match self.command_rx.try_recv() {
                Ok(ReaderCommand::GoTo(page)) => latest = Some(page.min(page_count - 1)),
                Ok(ReaderCommand::Next) => latest = Some((current + 1).min(page_count - 1)),
                Ok(ReaderCommand::Previous) => latest = Some(current.saturating_sub(1)),
                Ok(ReaderCommand::RequestThumbnails { center, radius }) => {
                    self.enqueue_thumbnails(center, radius, page_count);
                }
                Ok(ReaderCommand::Shutdown) => return Some(WindowResult::Shutdown),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Some(WindowResult::Shutdown),
            }
        }

        latest.map(WindowResult::JumpTo)
    }

    fn wait_for_command_if_idle(
        &mut self,
        current: PageIndex,
        page_count: usize,
    ) -> Option<WindowResult> {
        if !self.prefetch_queue.is_empty() || !self.thumbnail_queue.is_empty() {
            if let Ok(command) = self.command_rx.recv_timeout(Duration::from_millis(1)) {
                return self.apply_command(command, current, page_count);
            }
            return None;
        }

        match self.command_rx.recv() {
            Ok(command) => self.apply_command(command, current, page_count),
            Err(_) => Some(WindowResult::Shutdown),
        }
    }

    fn apply_command(
        &mut self,
        command: ReaderCommand,
        current: PageIndex,
        page_count: usize,
    ) -> Option<WindowResult> {
        match command {
            ReaderCommand::GoTo(page) => Some(WindowResult::JumpTo(page.min(page_count - 1))),
            ReaderCommand::Next => Some(WindowResult::JumpTo((current + 1).min(page_count - 1))),
            ReaderCommand::Previous => Some(WindowResult::JumpTo(current.saturating_sub(1))),
            ReaderCommand::RequestThumbnails { center, radius } => {
                self.enqueue_thumbnails(center, radius, page_count);
                Some(WindowResult::Continue)
            }
            ReaderCommand::Shutdown => Some(WindowResult::Shutdown),
        }
    }

    fn enqueue_thumbnails(&mut self, center: PageIndex, radius: usize, page_count: usize) {
        if page_count == 0 {
            return;
        }

        let center = center.min(page_count - 1);
        let start = center.saturating_sub(radius);
        let end = (center + radius).min(page_count - 1);

        for page in start..=end {
            if !self.thumbnail_cache.contains(page) && self.queued_thumbnails.insert(page) {
                self.thumbnail_queue.push_back(page);
            }
        }
    }

    fn emit_cache_stats(&self) {
        let _ = self.event_tx.send(ReaderEvent::CacheStats {
            raw_bytes: self.raw_cache.bytes_used(),
            display_bytes: self.display_cache.bytes_used(),
            thumbnail_bytes: self.thumbnail_cache.bytes_used(),
        });
    }

    fn emit_error(&self, error: ReaderError) {
        let _ = self.event_tx.send(ReaderEvent::Error(error.to_string()));
    }
}

enum WindowResult {
    Continue,
    JumpTo(PageIndex),
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_order_uses_natural_sorting_and_filters_images() {
        let order = PageOrder::from_entries(vec![
            ArchiveEntry {
                id: EntryId(0),
                name: "page10.jpg".to_string(),
                uncompressed_size: 1,
            },
            ArchiveEntry {
                id: EntryId(1),
                name: "notes.txt".to_string(),
                uncompressed_size: 1,
            },
            ArchiveEntry {
                id: EntryId(2),
                name: "page2.jpg".to_string(),
                uncompressed_size: 1,
            },
        ]);

        let names: Vec<_> = order
            .pages()
            .iter()
            .map(|page| page.name.as_str())
            .collect();
        assert_eq!(names, vec!["page2.jpg", "page10.jpg"]);
        assert_eq!(order.page(0).unwrap().entry_id, EntryId(2));
    }

    #[test]
    fn lru_cache_evicts_oldest_items_by_byte_budget() {
        let mut cache = LruPageCache::new(10);
        cache.insert(0, Bytes::from_static(b"12345"), 5);
        cache.insert(1, Bytes::from_static(b"67890"), 5);
        cache.insert(2, Bytes::from_static(b"abcde"), 5);

        assert!(!cache.contains(0));
        assert!(cache.contains(1));
        assert!(cache.contains(2));
        assert_eq!(cache.bytes_used(), 10);
    }
}
