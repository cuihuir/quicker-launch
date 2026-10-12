//! Bounded clipboard history with private, atomic persistence.
use crate::{config::atomic_write, search};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub const MAX_TEXT: usize = 256 * 1024;
pub const MAX_IMAGE: usize = 8 * 1024 * 1024;
const MAX_HISTORY_FILE: usize = 16 * 1024 * 1024;
const MAX_TOTAL: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Content {
    Text {
        text: String,
    },
    Image {
        hash: String,
        width: i32,
        height: i32,
        bytes: usize,
    },
}

impl Content {
    pub fn preview(&self) -> String {
        match self {
            Self::Text { text } => search::preview(text, 110),
            Self::Image { width, height, .. } => format!("图片 · {width} × {height}"),
        }
    }
    pub fn matches(&self, query: &str) -> bool {
        match self {
            Self::Text { text } => search::contains_terms(query, text),
            Self::Image { .. } => search::contains_terms(query, "图片 image png"),
        }
    }
    fn size(&self) -> usize {
        match self {
            Self::Text { text } => text.len(),
            Self::Image { bytes, .. } => *bytes,
        }
    }
    fn same_content(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text { text: a }, Self::Text { text: b }) => a == b,
            (Self::Image { hash: a, .. }, Self::Image { hash: b, .. }) => a == b,
            _ => false,
        }
    }
}

pub struct History {
    pub entries: Vec<Content>,
    directory: PathBuf,
    persistent: bool,
    memory_images: HashMap<String, Vec<u8>>,
}

impl History {
    pub fn open(directory: PathBuf, persist: bool, limit: usize) -> Result<Self> {
        fs::create_dir_all(directory.join("images"))?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(directory.join("images"), fs::Permissions::from_mode(0o700))?;
        let mut history = Self {
            entries: vec![],
            directory,
            persistent: persist,
            memory_images: HashMap::new(),
        };
        let file = history.directory.join("history.json");
        if persist && file.exists() {
            let mut bytes = Vec::new();
            fs::File::open(file)?
                .take((MAX_HISTORY_FILE + 1) as u64)
                .read_to_end(&mut bytes)?;
            if bytes.len() <= MAX_HISTORY_FILE {
                // Invalid entries are skipped; never print the private JSON.
                let entries: Vec<Content> =
                    serde_json::from_slice(&bytes).context("历史文件格式错误")?;
                history.entries = entries
                    .into_iter()
                    .filter(|entry| match entry {
                        Content::Text { text } => {
                            !text.is_empty() && text.len() <= MAX_TEXT && !text.contains('\0')
                        }
                        Content::Image {
                            hash,
                            width,
                            height,
                            bytes,
                        } => {
                            *width > 0
                                && *height > 0
                                && (*width as i64 * *height as i64) <= 16_000_000
                                && *bytes <= MAX_IMAGE
                                && history.image_path(hash).is_some()
                        }
                    })
                    .collect();
            }
        }
        history.trim(limit);
        Ok(history)
    }

    pub fn add_text(&mut self, text: String, limit: usize) -> bool {
        if text.is_empty() || text.len() > MAX_TEXT || text.contains('\0') {
            return false;
        }
        self.insert(Content::Text { text }, limit)
    }

    pub fn add_image(&mut self, png: &[u8], width: i32, height: i32, limit: usize) -> Result<bool> {
        if png.is_empty()
            || png.len() > MAX_IMAGE
            || width <= 0
            || height <= 0
            || width as i64 * height as i64 > 16_000_000
        {
            return Ok(false);
        }
        let hash = format!("{:x}", Sha256::digest(png));
        let path = self.directory.join("images").join(format!("{hash}.png"));
        if self.persistent {
            if !path.exists() {
                atomic_write(&path, png)?;
            }
        } else {
            self.memory_images.insert(hash.clone(), png.to_vec());
        }
        Ok(self.insert(
            Content::Image {
                hash,
                width,
                height,
                bytes: png.len(),
            },
            limit,
        ))
    }

    fn insert(&mut self, entry: Content, limit: usize) -> bool {
        if self
            .entries
            .first()
            .is_some_and(|item| item.same_content(&entry))
        {
            return false;
        }
        self.entries.retain(|item| !item.same_content(&entry));
        self.entries.insert(0, entry);
        self.trim(limit);
        true
    }

    pub fn trim(&mut self, limit: usize) {
        self.entries.truncate(limit);
        let mut text_bytes = 0;
        let mut total_bytes = 0;
        self.entries.retain(|entry| {
            total_bytes += entry.size();
            if let Content::Text { text } = entry {
                text_bytes += text.len();
            }
            // Reserve JSON escaping overhead inside the bounded state file.
            text_bytes <= MAX_HISTORY_FILE / 8 && total_bytes <= MAX_TOTAL
        });
        self.collect_unused_images();
    }

    pub fn save(&self, persist: bool) -> Result<()> {
        let path = self.directory.join("history.json");
        if persist {
            atomic_write(&path, &serde_json::to_vec(&self.entries)?)
        } else {
            if path.exists() {
                fs::remove_file(path)?;
            }
            Ok(())
        }
    }

    pub fn image_path(&self, hash: &str) -> Option<PathBuf> {
        if hash.len() != 64 || !hash.bytes().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let root = self.directory.join("images").canonicalize().ok()?;
        let path = root.join(format!("{hash}.png")).canonicalize().ok()?;
        let metadata = path.metadata().ok()?;
        (path.starts_with(root) && metadata.is_file() && metadata.len() <= MAX_IMAGE as u64)
            .then_some(path)
    }

    pub fn image_bytes(&self, hash: &str) -> Result<Vec<u8>> {
        if let Some(bytes) = self.memory_images.get(hash) {
            return Ok(bytes.clone());
        }
        let path = self.image_path(hash).context("图片已不存在")?;
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take((MAX_IMAGE + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_IMAGE {
            anyhow::bail!("图片超过大小限制");
        }
        Ok(bytes)
    }

    pub fn set_persistence(&mut self, enabled: bool) -> Result<()> {
        if enabled == self.persistent {
            return Ok(());
        }
        let mut images = HashMap::new();
        for item in &self.entries {
            if let Content::Image { hash, .. } = item {
                images.insert(hash.clone(), self.image_bytes(hash)?);
            }
        }
        if enabled {
            for (hash, bytes) in &images {
                atomic_write(
                    &self.directory.join("images").join(format!("{hash}.png")),
                    bytes,
                )?;
            }
            self.memory_images.clear();
        } else {
            self.memory_images = images;
        }
        self.persistent = enabled;
        self.collect_unused_images();
        self.save(enabled)
    }

    pub fn clear(&mut self) -> Result<()> {
        self.entries.clear();
        self.collect_unused_images();
        let path = self.directory.join("history.json");
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    fn collect_unused_images(&mut self) {
        self.memory_images.retain(|hash, _| {
            self.entries
                .iter()
                .any(|item| matches!(item, Content::Image { hash: h, .. } if h == hash))
        });
        let Ok(files) = fs::read_dir(self.directory.join("images")) else {
            return;
        };
        for file in files.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let Some(hash) = name.strip_suffix(".png") else {
                continue;
            };
            if hash.len() == 64
                && hash.bytes().all(|c| c.is_ascii_hexdigit())
                && (!self.persistent
                    || !self
                        .entries
                        .iter()
                        .any(|item| matches!(item, Content::Image { hash: h, .. } if h == hash)))
            {
                let _ = fs::remove_file(file.path());
            }
        }
    }

    /// A deliberate one-time import, independent of the old recorder thereafter.
    pub fn import_indicator(&mut self, source: &Path, limit: usize) -> Result<usize> {
        let mut bytes = Vec::new();
        fs::File::open(source)?
            .take((MAX_HISTORY_FILE + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_HISTORY_FILE {
            anyhow::bail!("旧历史文件过大");
        }
        let records: Vec<serde_json::Value> =
            serde_json::from_slice(&bytes).context("旧历史格式错误")?;
        let mut count = 0;
        for record in records {
            let Some(text) = record.get("contents").and_then(|value| value.as_str()) else {
                continue;
            };
            let mime = record
                .get("mimetype")
                .and_then(|value| value.as_str())
                .unwrap_or("text/plain");
            if (mime.starts_with("text/") || mime == "STRING" || mime == "UTF8_STRING")
                && self.add_text(text.to_owned(), limit)
            {
                count += 1;
            }
        }
        Ok(count)
    }
}

pub fn png_dimensions(bytes: &[u8]) -> Option<(i32, i32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    if width == 0 || height == 0 || width as u64 * height as u64 > 16_000_000 {
        return None;
    }
    Some((width as i32, height as i32))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> History {
        let path =
            std::env::temp_dir().join(format!("quick-launcher-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        History::open(path, false, 10).unwrap()
    }
    #[test]
    fn dedup_moves_to_front_without_changing_original_text() {
        let mut h = fixture("dedup");
        h.add_text(" old\ntext ".into(), 2);
        h.add_text("new".into(), 2);
        assert!(h.add_text(" old\ntext ".into(), 2));
        assert!(!h.add_text(" old\ntext ".into(), 2));
        assert_eq!(h.entries.len(), 2);
        assert!(matches!(&h.entries[0], Content::Text { text } if text == " old\ntext "));
        let _ = fs::remove_dir_all(h.directory);
    }
    #[test]
    fn persistence_permissions_paths_and_clear() {
        let mut h = fixture("persist");
        h.add_text("synthetic".into(), 10);
        h.save(true).unwrap();
        assert_eq!(
            fs::metadata(h.directory.join("history.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let reloaded = History::open(h.directory.clone(), true, 10).unwrap();
        assert_eq!(h.entries, reloaded.entries);
        assert!(h.image_path("../../outside").is_none());
        assert!(!h.add_text("x".repeat(MAX_TEXT + 1), 10));
        h.save(false).unwrap();
        assert!(!h.directory.join("history.json").exists());
        h.clear().unwrap();
        assert!(h.entries.is_empty());
        let _ = fs::remove_dir_all(h.directory);
    }
    #[test]
    fn eviction_removes_only_unused_owned_images() {
        let mut h = fixture("images");
        h.set_persistence(true).unwrap();
        h.add_image(b"fake png for model test", 1, 1, 1).unwrap();
        let Content::Image { hash, .. } = h.entries[0].clone() else {
            panic!()
        };
        let path = h.image_path(&hash).unwrap();
        h.add_text("new".into(), 1);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(h.directory);
    }
    #[test]
    fn disabled_persistence_keeps_images_only_in_memory() {
        let mut h = fixture("private-images");
        h.add_image(b"synthetic", 1, 1, 10).unwrap();
        let Content::Image { hash, .. } = h.entries[0].clone() else {
            panic!()
        };
        assert!(h.image_path(&hash).is_none());
        assert_eq!(h.image_bytes(&hash).unwrap(), b"synthetic");
        h.set_persistence(true).unwrap();
        assert!(h.image_path(&hash).is_some());
        h.set_persistence(false).unwrap();
        assert!(h.image_path(&hash).is_none());
        assert_eq!(h.image_bytes(&hash).unwrap(), b"synthetic");
        let _ = fs::remove_dir_all(h.directory);
    }
    #[test]
    fn png_header_rejects_large_images_before_decoding() {
        let mut header = vec![0; 24];
        header[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        header[12..16].copy_from_slice(b"IHDR");
        header[16..20].copy_from_slice(&1920_u32.to_be_bytes());
        header[20..24].copy_from_slice(&1080_u32.to_be_bytes());
        assert_eq!(png_dimensions(&header), Some((1920, 1080)));
        header[16..20].copy_from_slice(&100_000_u32.to_be_bytes());
        assert!(png_dimensions(&header).is_none());
    }
}
