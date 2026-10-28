//! Portrait normalization and the bounded image-handle cache.

/// Normalizes a PNG character portrait: verifies 128..=512 dimensions (max 4096 px source)
/// and crops horizontally centered to a top-aligned square if needed.
pub fn normalize_character_image_png(source: &[u8]) -> std::result::Result<Vec<u8>, String> {
    let decoded = ::image::load_from_memory_with_format(source, ::image::ImageFormat::Png)
        .map_err(|_| "must be a valid PNG".to_owned())?;
    let (width, height) = (decoded.width(), decoded.height());
    let side = width.min(height);
    if !(128..=512).contains(&side) || width > 4096 || height > 4096 {
        return Err(
            "the cropped portrait must be 128×128 to 512×512 (source sides may be up to 4096 px)"
                .into(),
        );
    }
    if width == height {
        return Ok(source.to_vec());
    }
    let left = (width - side) / 2;
    let cropped = decoded.crop_imm(left, 0, side, side);
    let mut output = std::io::Cursor::new(Vec::new());
    cropped
        .write_to(&mut output, ::image::ImageFormat::Png)
        .map_err(|_| "could not normalize PNG portrait".to_owned())?;
    Ok(output.into_inner())
}

pub(super) const MAX_CACHED_IMAGES: usize = 32;

pub(super) const MAX_CACHED_IMAGE_BYTES: usize = 8 * 1024 * 1024;

thread_local! {
    // Iced builds views on the UI thread. Reuse identities so its renderer can
    // reuse decoded images instead of decoding every rebuilt avatar.
    static IMAGE_HANDLES: std::cell::RefCell<CharacterImageCache> =
        std::cell::RefCell::new(CharacterImageCache::default());
}

/// Reuses a bounded, content-checked image handle for non-empty encoded bytes.
#[must_use]
pub fn image_handle(bytes: &[u8]) -> Option<iced::widget::image::Handle> {
    IMAGE_HANDLES.with_borrow_mut(|cache| cache.get_or_insert(bytes))
}

/// Releases retained image bytes when the account is cleared or changed.
pub fn clear_image_handles() {
    IMAGE_HANDLES.with_borrow_mut(CharacterImageCache::clear);
}

/// Least-recently-used encoded images, bounded by both bytes and entry count.
#[derive(Default, Debug, Clone)]
pub struct CharacterImageCache {
    pub(super) handles: std::collections::VecDeque<(u64, iced::widget::image::Handle)>,
    pub(super) retained_bytes: usize,
}

impl CharacterImageCache {
    pub fn get_or_insert(&mut self, bytes: &[u8]) -> Option<iced::widget::image::Handle> {
        use std::hash::{Hash, Hasher};

        if bytes.is_empty() {
            return None;
        }
        let mut hasher = std::hash::DefaultHasher::new();
        bytes.hash(&mut hasher);
        let hash = hasher.finish();
        if let Some(index) = self.handles.iter().position(|(key, handle)| {
            *key == hash && matches!(handle, iced::widget::image::Handle::Bytes(_, data) if data.as_ref() == bytes)
        }) {
            let entry = self.handles.remove(index).expect("cached image exists");
            let handle = entry.1.clone();
            self.handles.push_back(entry);
            return Some(handle);
        }

        let handle = iced::widget::image::Handle::from_bytes(bytes.to_vec());
        if bytes.len() <= MAX_CACHED_IMAGE_BYTES {
            while self.handles.len() >= MAX_CACHED_IMAGES
                || self.retained_bytes + bytes.len() > MAX_CACHED_IMAGE_BYTES
            {
                if let Some((_, iced::widget::image::Handle::Bytes(_, data))) =
                    self.handles.pop_front()
                {
                    self.retained_bytes -= data.len();
                }
            }
            self.retained_bytes += bytes.len();
            self.handles.push_back((hash, handle.clone()));
        }
        Some(handle)
    }

    pub fn clear(&mut self) {
        self.handles.clear();
        self.retained_bytes = 0;
    }
}

/// Crops a generated PNG to a top-aligned square and downsizes it to at most `max_side`.
pub fn generated_portrait_png(
    source: &[u8],
    max_side: u32,
) -> std::result::Result<Vec<u8>, String> {
    let decoded = ::image::load_from_memory_with_format(source, ::image::ImageFormat::Png)
        .map_err(|_| "the image service did not return a valid PNG".to_owned())?;
    let side = decoded.width().min(decoded.height());
    if side < 128 {
        return Err("the generated image is smaller than 128×128".into());
    }
    let square = decoded.crop_imm((decoded.width() - side) / 2, 0, side, side);
    let portrait = if side > max_side {
        square.resize_exact(max_side, max_side, ::image::imageops::FilterType::Triangle)
    } else {
        square
    };
    let mut output = std::io::Cursor::new(Vec::new());
    portrait
        .write_to(&mut output, ::image::ImageFormat::Png)
        .map_err(|_| "could not encode the generated portrait".to_owned())?;
    Ok(output.into_inner())
}
