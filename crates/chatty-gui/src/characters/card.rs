//! SillyTavern JSON/PNG cards and Chatty zip archives.

use super::{
    DraftCharacter, MAX_CHARACTER_IMAGE_BYTES, MAX_CHARACTER_IMAGES, normalize_character_image_png,
};
use chatty_protocol::CharacterImage;
use std::collections::HashSet;

/// Parse a SillyTavern JSON v2 character card or fallback JSON object into a `DraftCharacter`.
#[must_use]
pub fn draft_from_card(root: serde_json::Value) -> Option<DraftCharacter> {
    let d = root.get("data").unwrap_or(&root);
    let ext = d.pointer("/extensions/chatty");
    Some(DraftCharacter {
        id: None,
        name: d.get("name")?.as_str()?.into(),
        description: str_field(d, "description"),
        personality: str_field(d, "personality"),
        scenario: str_field(d, "scenario"),
        system_prompt: str_field(d, "system_prompt"),
        example_dialogue: str_field(d, "mes_example"),
        appearance: ext.map(|e| str_field(e, "appearance")).unwrap_or_default(),
        age: ext.map(|e| str_field(e, "age")).unwrap_or_default(),
        gender: ext.map(|e| str_field(e, "gender")).unwrap_or_default(),
        race: ext.map(|e| str_field(e, "race")).unwrap_or_default(),
        misc: ext.map(|e| str_field(e, "misc")).unwrap_or_default(),
        tags: d
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        avatar: None,
        images: vec![],
        default_image_id: None,
        is_public: false,
        owned_by_user: true,
    })
}

pub(super) fn str_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .into()
}

/// Export a `DraftCharacter` to a SillyTavern JSON v2 character card object.
#[must_use]
pub fn export_card_json(draft: &DraftCharacter) -> serde_json::Value {
    let tags: Vec<&str> = draft
        .tags
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let images: Vec<_> = draft
        .images
        .iter()
        .zip(image_file_names(&draft.images))
        .map(|(image, file)| {
            serde_json::json!({
                "file": file,
                "label": image.label,
                "default": draft.default_image_id.as_deref() == Some(image.id.as_str()),
            })
        })
        .collect();
    serde_json::json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "data": {
            "name": draft.name,
            "description": draft.description,
            "personality": draft.personality,
            "scenario": draft.scenario,
            "first_mes": "",
            "mes_example": draft.example_dialogue,
            "system_prompt": draft.system_prompt,
            "tags": tags,
            "extensions": {
                "chatty": {
                    "appearance": draft.appearance,
                    "age": draft.age,
                    "gender": draft.gender,
                    "race": draft.race,
                    "misc": draft.misc,
                    "images": images,
                }
            }
        }
    })
}

pub(super) const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Card file inside a character archive.
pub(super) const ARCHIVE_CARD: &str = "character.json";

/// Upper bound for the card JSON read from an archive.
pub(super) const MAX_ARCHIVE_CARD_BYTES: u64 = 4 * 1024 * 1024;

/// Archive paths for the draft's images, e.g. `images/happy.png`, named after
/// each label. A clashing name gets a numeric suffix: `images/happy-2.png`.
pub(super) fn image_file_names(images: &[CharacterImage]) -> Vec<String> {
    let mut used = HashSet::new();
    images
        .iter()
        .map(|image| {
            let stem: String = image
                .label
                .trim()
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .take(40)
                .collect();
            let stem = if stem.is_empty() {
                "image".to_owned()
            } else {
                stem
            };
            let mut name = stem.clone();
            let mut suffix = 1;
            // Compare case-insensitively so archives unpack cleanly on any file system.
            while !used.insert(name.to_lowercase()) {
                suffix += 1;
                name = format!("{stem}-{suffix}");
            }
            format!("images/{name}.png")
        })
        .collect()
}

/// Exports the draft as a zip archive: `character.json`, a SillyTavern v2 card
/// whose `extensions.chatty.images` lists every image, and the PNGs under `images/`.
pub fn export_card_zip(draft: &DraftCharacter) -> Result<Vec<u8>, String> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let fail = |error: &dyn std::fmt::Display| error.to_string();
    let json = serde_json::to_vec_pretty(&export_card_json(draft)).map_err(|e| fail(&e))?;
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    archive
        .start_file(ARCHIVE_CARD, SimpleFileOptions::default())
        .map_err(|e| fail(&e))?;
    archive.write_all(&json).map_err(|e| fail(&e))?;
    // PNGs are already compressed; storing them avoids wasted work.
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (image, file) in draft.images.iter().zip(image_file_names(&draft.images)) {
        archive.start_file(file, stored).map_err(|e| fail(&e))?;
        archive.write_all(&image.data).map_err(|e| fail(&e))?;
    }
    let out = archive.finish().map_err(|e| fail(&e))?;
    Ok(out.into_inner())
}

/// Reads an archive written by [`export_card_zip`] into a new draft.
///
/// `character.json` may sit at the root or in one top-level folder. Images are
/// normalized like manual uploads and bounded by the character image limits.
#[must_use]
pub fn draft_from_zip(bytes: &[u8]) -> Option<DraftCharacter> {
    use std::io::Read;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?;
    let card_path = archive
        .file_names()
        .filter(|name| *name == ARCHIVE_CARD || name.ends_with(&format!("/{ARCHIVE_CARD}")))
        .min_by_key(|name| name.len())?
        .to_owned();
    let folder = &card_path[..card_path.len() - ARCHIVE_CARD.len()];
    let mut read = |path: &str, limit: u64| -> Option<Vec<u8>> {
        let entry = archive.by_name(path).ok()?;
        if entry.size() > limit {
            return None;
        }
        let mut data = Vec::new();
        entry.take(limit).read_to_end(&mut data).ok()?;
        Some(data)
    };
    let card: serde_json::Value =
        serde_json::from_slice(&read(&card_path, MAX_ARCHIVE_CARD_BYTES)?).ok()?;
    let mut draft = draft_from_card(card.clone())?;
    let entries = card
        .pointer("/data/extensions/chatty/images")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .take(MAX_CHARACTER_IMAGES);
    let mut budget = MAX_CHARACTER_IMAGE_BYTES;
    for entry in entries {
        let Some(file) = entry.get("file").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(source) = read(&format!("{folder}{file}"), budget as u64) else {
            continue;
        };
        let Ok(data) = normalize_character_image_png(&source) else {
            continue;
        };
        if data.len() > budget {
            continue;
        }
        budget -= data.len();
        let id = chatty_protocol::util::new_uuid();
        if entry.get("default").and_then(|v| v.as_bool()) == Some(true) {
            draft.default_image_id = Some(id.clone());
        }
        draft.images.push(CharacterImage {
            id,
            label: str_field(entry, "label"),
            data,
        });
    }
    if draft.default_image_id.is_none() {
        draft.default_image_id = draft.images.first().map(|image| image.id.clone());
    }
    Some(draft)
}

/// Extracts and base64-decodes the `tEXt` chunk with keyword `chara` from PNG image bytes.
#[must_use]
pub fn png_card_json(bytes: &[u8]) -> Option<Vec<u8>> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let mut at = 8;
    while at + 12 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
        let kind = &bytes[at + 4..at + 8];
        if at + 12 + len > bytes.len() {
            return None;
        }
        if kind == b"tEXt" {
            let data = &bytes[at + 8..at + 8 + len];
            if let Some(zero) = data.iter().position(|b| *b == 0)
                && &data[..zero] == b"chara"
            {
                let text = std::str::from_utf8(&data[zero + 1..]).ok()?;
                return chatty_protocol::util::base64::decode(text);
            }
        }
        at += 12 + len;
    }
    None
}
