use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use bytes::Bytes;
use reader_core::{
    is_supported_image_name, ArchiveBackend, ArchiveEntry, EntryId, ReaderError, Result,
};
use walkdir::WalkDir;
use zip::ZipArchive;

pub struct ZipArchiveBackend {
    path: PathBuf,
}

impl ZipArchiveBackend {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn open_archive(&self) -> Result<ZipArchive<File>> {
        let file = File::open(&self.path).map_err(|error| {
            ReaderError::Archive(format!("failed to open {}: {error}", self.path.display()))
        })?;

        ZipArchive::new(file).map_err(|error| {
            ReaderError::Archive(format!("failed to parse {}: {error}", self.path.display()))
        })
    }
}

impl ArchiveBackend for ZipArchiveBackend {
    fn list_entries(&mut self) -> Result<Vec<ArchiveEntry>> {
        let mut archive = self.open_archive()?;
        let mut entries = Vec::new();

        for index in 0..archive.len() {
            let file = archive.by_index(index).map_err(|error| {
                ReaderError::Archive(format!("failed to read zip entry {index}: {error}"))
            })?;

            if file.is_dir() {
                continue;
            }

            let name = file.name().to_string();
            if !is_supported_image_name(&name) {
                continue;
            }

            entries.push(ArchiveEntry {
                id: EntryId(index),
                name,
                uncompressed_size: file.size(),
            });
        }

        Ok(entries)
    }

    fn read_entry(&mut self, entry_id: EntryId) -> Result<Bytes> {
        let mut archive = self.open_archive()?;
        let mut file = archive.by_index(entry_id.0).map_err(|error| {
            ReaderError::Archive(format!("failed to read zip entry {}: {error}", entry_id.0))
        })?;

        let mut buffer = Vec::with_capacity(file.size().min(usize::MAX as u64) as usize);
        file.read_to_end(&mut buffer).map_err(|error| {
            ReaderError::Archive(format!(
                "failed to extract zip entry {}: {error}",
                entry_id.0
            ))
        })?;

        Ok(Bytes::from(buffer))
    }
}

#[derive(Debug, Clone)]
pub struct FolderArchiveBackend {
    root: PathBuf,
    entries: Vec<PathBuf>,
}

impl FolderArchiveBackend {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            root: path.into(),
            entries: Vec::new(),
        }
    }
}

impl ArchiveBackend for FolderArchiveBackend {
    fn list_entries(&mut self) -> Result<Vec<ArchiveEntry>> {
        let mut files = Vec::new();

        for entry in WalkDir::new(&self.root).follow_links(true) {
            let entry = entry.map_err(|error| {
                ReaderError::Archive(format!(
                    "failed to scan folder {}: {error}",
                    self.root.display()
                ))
            })?;

            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path().to_path_buf();
            let name = path
                .strip_prefix(&self.root)
                .unwrap_or(path.as_path())
                .to_string_lossy()
                .replace('\\', "/");

            if is_supported_image_name(&name) {
                files.push(path);
            }
        }

        self.entries = files;

        Ok(self
            .entries
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let name = path
                    .strip_prefix(&self.root)
                    .unwrap_or(path.as_path())
                    .to_string_lossy()
                    .replace('\\', "/");
                let uncompressed_size = path.metadata().map(|metadata| metadata.len()).unwrap_or(0);

                ArchiveEntry {
                    id: EntryId(index),
                    name,
                    uncompressed_size,
                }
            })
            .collect())
    }

    fn read_entry(&mut self, entry_id: EntryId) -> Result<Bytes> {
        let path = self.entries.get(entry_id.0).ok_or_else(|| {
            ReaderError::Archive(format!("folder entry {} is out of range", entry_id.0))
        })?;

        std::fs::read(path).map(Bytes::from).map_err(|error| {
            ReaderError::Archive(format!("failed to read {}: {error}", path.display()))
        })
    }
}

pub fn backend_for_path(path: impl AsRef<Path>) -> Result<Box<dyn ArchiveBackend>> {
    let path = path.as_ref();

    if path.is_dir() {
        return Ok(Box::new(FolderArchiveBackend::new(path)));
    }

    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("zip" | "cbz") => Ok(Box::new(ZipArchiveBackend::new(path))),
        _ => Err(ReaderError::Archive(format!(
            "unsupported comic path {}",
            path.display()
        ))),
    }
}
