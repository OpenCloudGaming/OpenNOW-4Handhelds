//! The signed-in account's library as shown on the home shelf, and the box
//! art for it. Art is resolved through the core's artwork cache, decoded in
//! the background, and kept as textures only near the current selection.

use crate::artwork::{self, Decoder};
use crate::gl::{Painter, Texture};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

/// Textures kept alive at once: 32 × 256² RGBA is 8 MiB of GPU memory.
const TEXTURE_BUDGET: usize = 32;
/// Requested artwork width. Tiles are at most a few hundred pixels across.
const ARTWORK_WIDTH: &str = ";w=400";

/// One store version of a game, such as Steam or the NVIDIA bundle.
#[derive(Clone)]
pub struct StoreVersion {
    pub id: String,
    pub store: String,
    pub in_library: bool,
    pub selected: bool,
}

pub struct Game {
    pub id: String,
    pub variant_id: String,
    pub title: String,
    pub store: String,
    pub status: String,
    pub in_library: bool,
    pub versions: Vec<StoreVersion>,
    pub color: [f32; 4],
    pub artwork: Option<String>,
}

impl Game {
    pub fn parse(value: &Value) -> Option<Self> {
        let title = value["title"].as_str()?.trim().to_owned();
        if title.is_empty() {
            return None;
        }
        let variants = value["variants"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let selected = value["selectedVariantIndex"].as_u64().unwrap_or(0) as usize;
        let versions = variants
            .iter()
            .filter_map(|variant| {
                let id = variant["id"].as_str().filter(|id| !id.is_empty())?.to_owned();
                let store = variant["store"]
                    .as_str()
                    .or_else(|| variant["appStore"].as_str())
                    .map(store_label)
                    .unwrap_or_default();
                let status = variant["libraryStatus"]
                    .as_str()
                    .or_else(|| variant["gfn"]["library"]["status"].as_str());
                let in_library = variant["inLibrary"].as_bool().unwrap_or_else(|| {
                    matches!(status, Some("MANUAL" | "PLATFORM_SYNC" | "IN_LIBRARY"))
                });
                let selected = variant["librarySelected"]
                    .as_bool()
                    .or_else(|| variant["gfn"]["library"]["selected"].as_bool())
                    .unwrap_or(false);
                Some(StoreVersion { id, store, in_library, selected })
            })
            .collect::<Vec<_>>();
        let variant = variants.get(selected).or_else(|| variants.first());
        let id = value["id"].as_str().unwrap_or("").to_owned();
        let variant_id = variant.and_then(|variant| variant["id"].as_str()).unwrap_or("").to_owned();
        let in_library = variant.and_then(|variant| variant["inLibrary"].as_bool()).unwrap_or(false);
        let store = variant.and_then(|variant| variant["store"].as_str()).map(store_label).unwrap_or_default();
        let status = match variant.and_then(|variant| variant["gfnStatus"].as_str()) {
            Some("AVAILABLE") | None => "Ready to play".to_owned(),
            Some(other) => sentence_case(other),
        };
        let artwork = ["keyArtUrl", "imageUrl", "heroImageUrl"]
            .iter()
            .find_map(|key| value[*key].as_str().filter(|url| !url.is_empty()))
            .map(|url| url.replace(";w=900", ARTWORK_WIDTH).replace(";w=1200", ARTWORK_WIDTH));
        Some(Self { id, variant_id, color: placeholder_color(&title), title, store, status, in_library, versions, artwork })
    }

    /// Replaces the store list from a full catalog game, which includes every variant.
    pub fn apply_detail(&mut self, value: &Value) {
        let Some(detail) = Self::parse(value) else {
            return;
        };
        if detail.versions.is_empty() {
            return;
        }
        self.versions = detail.versions;
        if let Some(current) = self.versions.iter().find(|version| version.selected) {
            self.variant_id = current.id.clone();
            self.store = current.store.clone();
            self.in_library = current.in_library;
        }
    }
}

fn store_label(store: &str) -> String {
    match store {
        "STEAM" => "STEAM".into(),
        "EPIC" => "EPIC GAMES".into(),
        "XBOX" | "MICROSOFT" => "XBOX".into(),
        "UPLAY" | "UBISOFT" => "UBISOFT".into(),
        "BATTLENET" => "BATTLE.NET".into(),
        "EA_APP" | "ORIGIN" => "EA".into(),
        "GOG" => "GOG".into(),
        other => other.replace('_', " "),
    }
}

fn sentence_case(value: &str) -> String {
    let lower = value.replace('_', " ").to_lowercase();
    let mut characters = lower.chars();
    characters.next().map(|first| first.to_uppercase().chain(characters).collect()).unwrap_or_default()
}

/// A stable muted colour per title, shown until art arrives or when it is absent.
fn placeholder_color(title: &str) -> [f32; 4] {
    let hash = title.bytes().fold(2166136261u32, |hash, byte| (hash ^ byte as u32).wrapping_mul(16777619));
    let hue = (hash % 360) as f32 / 60.0;
    let (saturation, value) = (0.55, 0.42);
    let chroma = value * saturation;
    let x = chroma * (1.0 - (hue % 2.0 - 1.0).abs());
    let (r, g, b) = match hue as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let base = value - chroma;
    [r + base, g + base, b + base, 1.0]
}

enum Art {
    Resolving,
    Decoding,
    Ready(Texture),
    Failed,
}

/// Box-art state keyed by source URL.
pub struct Artwork {
    entries: HashMap<String, Art>,
    decoder: Decoder,
}

impl Artwork {
    pub fn new() -> Self {
        Self { entries: HashMap::new(), decoder: Decoder::new() }
    }

    pub fn texture(&self, url: &str) -> Option<&Texture> {
        match self.entries.get(url) {
            Some(Art::Ready(texture)) => Some(texture),
            _ => None,
        }
    }

    /// Whether `url` still needs an `artwork.resolve` request. Marks it as
    /// resolving when it does.
    pub fn begin_resolve(&mut self, url: &str) -> bool {
        if self.entries.contains_key(url) {
            return false;
        }
        self.entries.insert(url.to_owned(), Art::Resolving);
        true
    }

    /// Handles an `artwork.resolve` result or `artwork.ready` event.
    pub fn resolved(&mut self, payload: &Value) {
        let Some(url) = payload["sourceUrl"].as_str() else {
            return;
        };
        if !matches!(self.entries.get(url), Some(Art::Resolving)) {
            return;
        }
        match (payload["status"].as_str(), payload["filePath"].as_str()) {
            (Some("pending"), _) => {}
            (_, Some(path)) if payload["cached"].as_bool() == Some(true) => {
                self.entries.insert(url.to_owned(), Art::Decoding);
                self.decoder.submit(url.to_owned(), PathBuf::from(path));
            }
            _ => {
                self.entries.insert(url.to_owned(), Art::Failed);
            }
        }
    }

    pub fn resolve_failed(&mut self, url: &str) {
        self.entries.insert(url.to_owned(), Art::Failed);
    }

    /// Uploads finished decodes, then frees textures beyond the budget,
    /// keeping the ones listed in `keep` (nearest the selection first).
    pub fn update(&mut self, painter: &mut Painter, keep: &[&str]) {
        for decoded in self.decoder.finished() {
            if !matches!(self.entries.get(&decoded.key), Some(Art::Decoding)) {
                continue;
            }
            let entry = match decoded.pixels {
                Some(pixels) => Art::Ready(painter.upload(artwork::SIZE, artwork::SIZE, &pixels)),
                None => Art::Failed,
            };
            self.entries.insert(decoded.key, entry);
        }
        let ready = self.entries.values().filter(|art| matches!(art, Art::Ready(_))).count();
        if ready <= TEXTURE_BUDGET {
            return;
        }
        let keep: Vec<&str> = keep.iter().take(TEXTURE_BUDGET).copied().collect();
        let evict: Vec<String> = self
            .entries
            .iter()
            .filter(|(url, art)| matches!(art, Art::Ready(_)) && !keep.contains(&url.as_str()))
            .map(|(url, _)| url.clone())
            .take(ready - TEXTURE_BUDGET)
            .collect();
        for url in evict {
            if let Some(Art::Ready(texture)) = self.entries.remove(&url) {
                painter.release(texture);
            }
        }
    }

    pub fn clear(&mut self, painter: &mut Painter) {
        for (_, art) in self.entries.drain() {
            if let Art::Ready(texture) = art {
                painter.release(texture);
            }
        }
    }
}
