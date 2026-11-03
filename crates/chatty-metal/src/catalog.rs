//! Read-only index of where every weight lives on the model drive.
//!
//! Only safetensors headers are read. Model files are untrusted input, so sizes
//! are bounded and every extent is checked against its file before use.

use crate::{Error, Result, invalid};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File},
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

/// Larger than any real shard header (the whole model's index is about 7.5 MiB).
const MAX_JSON_BYTES: u64 = 64 << 20;
/// Weight and scale for each of an expert's w1, w2 and w3 matrices.
pub const EXPERT_EXTENTS: usize = 6;

/// The fields of `inference/config.json` that shape storage and memory.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelConfig {
    pub n_layers: usize,
    pub n_routed_experts: usize,
    pub n_activated_experts: usize,
    pub head_dim: u64,
    pub index_head_dim: u64,
    pub rope_head_dim: u64,
    pub kv_source_layers: Vec<usize>,
    pub compress_ratios: Vec<u64>,
    pub engram_layer_ids: Vec<usize>,
    pub engram_num_embeddings: Vec<u64>,
    pub engram_head_dim: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Extent {
    pub shard: u16,
    /// Absolute byte offset in the shard file.
    pub offset: u64,
    pub len: u64,
}

/// One layer's n-gram hash table, addressed by row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngramTable {
    pub layer: usize,
    pub rows: u64,
    pub weight: Extent,
    pub scale: Extent,
}

impl EngramTable {
    /// The weight and scale bytes of one row, or `None` past the end of the table.
    pub fn row(&self, row: u64) -> Option<(Extent, Extent)> {
        (row < self.rows).then(|| {
            (
                row_extent(self.weight, self.rows, row),
                row_extent(self.scale, self.rows, row),
            )
        })
    }
}

fn row_extent(table: Extent, rows: u64, row: u64) -> Extent {
    let len = table.len / rows;
    Extent {
        shard: table.shard,
        offset: table.offset + row * len,
        len,
    }
}

/// Storage element type in safetensors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dtype {
    Bf16,
    F32,
    F8E4m3,
    F8E8m0,
    I8,
}

impl Dtype {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "BF16" => Some(Self::Bf16),
            "F32" => Some(Self::F32),
            "F8_E4M3" => Some(Self::F8E4m3),
            "F8_E8M0" => Some(Self::F8E8m0),
            "I8" => Some(Self::I8),
            _ => None,
        }
    }

    pub const fn bytes(self) -> u64 {
        match self {
            Self::Bf16 => 2,
            Self::F32 => 4,
            Self::F8E4m3 | Self::F8E8m0 | Self::I8 => 1,
        }
    }
}

/// One core backbone tensor that stays resident in VRAM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreTensor {
    pub dtype: Dtype,
    pub shape: Vec<u64>,
    pub extent: Extent,
}

/// Bytes on disk per part of the model.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Sizes {
    /// Always needed for text generation; lives in VRAM.
    pub core: u64,
    pub experts: u64,
    pub engram: u64,
    /// Not needed for plain autoregressive text generation.
    pub mtp: u64,
    pub vision: u64,
}

#[derive(Debug)]
pub struct Catalog {
    pub config: ModelConfig,
    pub shards: Vec<PathBuf>,
    pub sizes: Sizes,
    /// Bytes of the largest routed expert, used as the cache slot size.
    pub expert_slot: u64,
    pub core: BTreeMap<String, CoreTensor>,
    pub engram: Vec<EngramTable>,
    experts: Vec<[Extent; EXPERT_EXTENTS]>,
}

impl Catalog {
    /// Index a model directory. Files are opened read-only and never mapped writable.
    pub fn open(dir: &Path) -> Result<Self> {
        let config: ModelConfig = read_json(&dir.join("inference/config.json"))?;
        let index: Index = read_json(&dir.join("model.safetensors.index.json"))?;
        if config.engram_layer_ids.len() != config.engram_num_embeddings.len() {
            return invalid("engram_layer_ids and engram_num_embeddings differ in length");
        }

        let names: BTreeSet<&str> = index.weight_map.values().map(String::as_str).collect();
        if names.len() > u16::MAX as usize {
            return invalid("too many shards");
        }
        let expert_count = config.n_layers * config.n_routed_experts;
        let mut build = Build {
            config: &config,
            weight_map: &index.weight_map,
            sizes: Sizes::default(),
            core: BTreeMap::new(),
            experts: vec![[None; EXPERT_EXTENTS]; expert_count],
            engram: HashMap::new(),
            seen: 0,
        };
        let mut shards = Vec::with_capacity(names.len());
        for (shard, name) in names.iter().enumerate() {
            // The index is untrusted: a shard name must not leave the model directory.
            if name.is_empty() || name.contains(['/', '\\']) || *name == ".." || *name == "." {
                return invalid(format!("unsafe shard name {name:?}"));
            }
            let path = dir.join(name);
            build.shard(shard as u16, name, &path)?;
            shards.push(path);
        }
        if build.seen != index.weight_map.len() {
            return invalid(format!(
                "index lists {} tensors but shards hold {}",
                index.weight_map.len(),
                build.seen
            ));
        }
        let (sizes, core, experts, engram) = build.finish()?;
        let expert_slot = experts
            .iter()
            .map(|extents| extents.iter().map(|extent| extent.len).sum())
            .max()
            .unwrap_or(0);
        Ok(Self {
            config,
            shards,
            sizes,
            expert_slot,
            core,
            engram,
            experts,
        })
    }

    pub fn expert_count(&self) -> usize {
        self.experts.len()
    }

    pub fn expert(&self, layer: usize, expert: usize) -> Option<&[Extent; EXPERT_EXTENTS]> {
        if expert >= self.config.n_routed_experts {
            return None;
        }
        self.experts
            .get(layer.checked_mul(self.config.n_routed_experts)? + expert)
    }
}

#[derive(Deserialize)]
struct Index {
    weight_map: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct TensorEntry {
    dtype: String,
    shape: Vec<u64>,
    data_offsets: [u64; 2],
}

struct Build<'a> {
    config: &'a ModelConfig,
    weight_map: &'a BTreeMap<String, String>,
    sizes: Sizes,
    core: BTreeMap<String, CoreTensor>,
    experts: Vec<[Option<Extent>; EXPERT_EXTENTS]>,
    /// Layer to (weight, scale) as (extent, rows).
    engram: HashMap<usize, [Option<(Extent, u64)>; 2]>,
    seen: usize,
}

impl Build<'_> {
    fn shard(&mut self, shard: u16, name: &str, path: &Path) -> Result<()> {
        let io_error = |error| Error::Io(path.to_owned(), error);
        let file = File::open(path).map_err(io_error)?;
        let file_len = file.metadata().map_err(io_error)?.len();
        let mut reader = BufReader::new(file);
        let mut prefix = [0u8; 8];
        reader.read_exact(&mut prefix).map_err(io_error)?;
        let header_len = u64::from_le_bytes(prefix);
        if header_len > MAX_JSON_BYTES {
            return invalid(format!(
                "{name}: header of {header_len} bytes exceeds limit"
            ));
        }
        let data_start = 8 + header_len;
        if data_start > file_len {
            return invalid(format!("{name}: header runs past end of file"));
        }
        let mut header = vec![0; header_len as usize];
        reader.read_exact(&mut header).map_err(io_error)?;
        let tensors: BTreeMap<String, serde_json::Value> = serde_json::from_slice(&header)
            .map_err(|error| Error::Invalid(format!("{name}: {error}")))?;

        for (tensor, value) in tensors {
            if tensor == "__metadata__" {
                continue;
            }
            let entry: TensorEntry = serde_json::from_value(value)
                .map_err(|error| Error::Invalid(format!("{name}: {tensor}: {error}")))?;
            if self.weight_map.get(&tensor).map(String::as_str) != Some(name) {
                return invalid(format!(
                    "{tensor} is in {name}, which the index does not name"
                ));
            }
            let (dtype, extent) = extent(shard, &tensor, &entry, data_start, file_len)?;
            self.seen += 1;
            self.place(&tensor, dtype, &entry, extent)?;
        }
        Ok(())
    }

    fn place(
        &mut self,
        name: &str,
        dtype: Dtype,
        entry: &TensorEntry,
        extent: Extent,
    ) -> Result<()> {
        if name.starts_with("mtp.") {
            self.sizes.mtp += extent.len;
        } else if name.starts_with("vision.")
            || name.starts_with("aligner.")
            || name.starts_with("image_")
        {
            self.sizes.vision += extent.len;
        } else if name.contains(".ffn.experts.") {
            let Some((layer, expert, slot)) = parse_expert(name) else {
                return invalid(format!("unexpected expert tensor {name}"));
            };
            if layer >= self.config.n_layers || expert >= self.config.n_routed_experts {
                return invalid(format!("{name} is outside the configured model"));
            }
            let target = &mut self.experts[layer * self.config.n_routed_experts + expert][slot];
            if target.replace(extent).is_some() {
                return invalid(format!("duplicate tensor {name}"));
            }
            self.sizes.experts += extent.len;
        } else if let Some((layer, kind)) = parse_engram_table(name) {
            let rows = entry.shape.first().copied().unwrap_or(0);
            let tables = self.engram.entry(layer).or_default();
            if tables[kind].replace((extent, rows)).is_some() {
                return invalid(format!("duplicate tensor {name}"));
            }
            self.sizes.engram += extent.len;
        } else {
            self.sizes.core += extent.len;
            if self
                .core
                .insert(
                    name.to_owned(),
                    CoreTensor {
                        dtype,
                        shape: entry.shape.clone(),
                        extent,
                    },
                )
                .is_some()
            {
                return invalid(format!("duplicate tensor {name}"));
            }
        }
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    fn finish(
        self,
    ) -> Result<(
        Sizes,
        BTreeMap<String, CoreTensor>,
        Vec<[Extent; EXPERT_EXTENTS]>,
        Vec<EngramTable>,
    )> {
        let mut experts = Vec::with_capacity(self.experts.len());
        for (index, slots) in self.experts.iter().enumerate() {
            let mut extents = [Extent::default(); EXPERT_EXTENTS];
            for (extent, slot) in extents.iter_mut().zip(slots) {
                let Some(found) = slot else {
                    return invalid(format!("expert {index} is missing a weight"));
                };
                *extent = *found;
            }
            experts.push(extents);
        }

        let mut engram = Vec::with_capacity(self.config.engram_layer_ids.len());
        for (&layer, &rows) in self
            .config
            .engram_layer_ids
            .iter()
            .zip(&self.config.engram_num_embeddings)
        {
            let tables = self.engram.get(&layer);
            let (Some((weight, weight_rows)), Some((scale, scale_rows))) =
                (tables.and_then(|t| t[0]), tables.and_then(|t| t[1]))
            else {
                return invalid(format!("layer {layer} is missing its engram table"));
            };
            if rows == 0 || weight_rows != rows || scale_rows != rows {
                return invalid(format!(
                    "layer {layer} engram table does not have {rows} rows"
                ));
            }
            if weight.len != rows * self.config.engram_head_dim {
                return invalid(format!(
                    "layer {layer} engram rows are not engram_head_dim wide"
                ));
            }
            engram.push(EngramTable {
                layer,
                rows,
                weight,
                scale,
            });
        }
        Ok((self.sizes, self.core, experts, engram))
    }
}

fn extent(
    shard: u16,
    name: &str,
    entry: &TensorEntry,
    data_start: u64,
    file_len: u64,
) -> Result<(Dtype, Extent)> {
    let Some(dtype) = Dtype::parse(entry.dtype.as_str()) else {
        return invalid(format!("{name}: unsupported dtype {}", entry.dtype));
    };
    let element = dtype.bytes();
    let [begin, end] = entry.data_offsets;
    let expected = entry
        .shape
        .iter()
        .try_fold(element, |bytes: u64, &dim| bytes.checked_mul(dim));
    if end < begin || expected != Some(end - begin) {
        return invalid(format!("{name}: offsets do not match dtype and shape"));
    }
    let offset = data_start.checked_add(begin);
    if data_start
        .checked_add(end)
        .is_none_or(|absolute| absolute > file_len)
    {
        return invalid(format!("{name}: data runs past end of file"));
    }
    let Some(offset) = offset else {
        return invalid(format!("{name}: offset overflow"));
    };
    Ok((
        dtype,
        Extent {
            shard,
            offset,
            len: end - begin,
        },
    ))
}

/// `layers.L.ffn.experts.E.wN.{weight,scale}` to (L, E, slot).
fn parse_expert(name: &str) -> Option<(usize, usize, usize)> {
    let mut parts = name.split('.');
    (parts.next()? == "layers").then_some(())?;
    let layer = parts.next()?.parse().ok()?;
    (parts.next()? == "ffn" && parts.next()? == "experts").then_some(())?;
    let expert = parts.next()?.parse().ok()?;
    let matrix = match parts.next()? {
        "w1" => 0,
        "w2" => 1,
        "w3" => 2,
        _ => return None,
    };
    let kind = match parts.next()? {
        "weight" => 0,
        "scale" => 1,
        _ => return None,
    };
    parts
        .next()
        .is_none()
        .then_some((layer, expert, matrix * 2 + kind))
}

/// `layers.L.engram.embed.{weight,scale}` to (L, 0 for weight or 1 for scale).
fn parse_engram_table(name: &str) -> Option<(usize, usize)> {
    let (layer, table) = name.strip_prefix("layers.")?.split_once(".engram.embed.")?;
    let kind = match table {
        "weight" => 0,
        "scale" => 1,
        _ => return None,
    };
    Some((layer.parse().ok()?, kind))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let io_error = |error| Error::Io(path.to_owned(), error);
    let len = fs::metadata(path).map_err(io_error)?.len();
    if len > MAX_JSON_BYTES {
        return invalid(format!("{}: {len} bytes exceeds limit", path.display()));
    }
    let bytes = fs::read(path).map_err(io_error)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| Error::Invalid(format!("{}: {error}", path.display())))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicU32, Ordering};

    pub(crate) fn config() -> Value {
        json!({
            "n_layers": 2, "n_routed_experts": 2, "n_activated_experts": 1,
            "head_dim": 8, "index_head_dim": 4, "rope_head_dim": 4,
            "kv_source_layers": [1], "compress_ratios": [0, 2],
            "engram_layer_ids": [1], "engram_num_embeddings": [3], "engram_head_dim": 4,
        })
    }

    pub(crate) struct Fixture(pub PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Writes one model directory. `edit` changes the tensor list before layout.
    pub(crate) fn fixture(
        edit: impl FnOnce(&mut Vec<(String, &'static str, Vec<u64>)>),
    ) -> Fixture {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "chatty-metal-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(dir.join("inference")).unwrap();
        fs::write(dir.join("inference/config.json"), config().to_string()).unwrap();

        let mut tensors = vec![
            ("embed.weight".to_owned(), "BF16", vec![4, 2]),
            ("mtp.0.attn.wkv.weight".to_owned(), "F8_E4M3", vec![5]),
            ("vision.blocks.0.attn.wo.weight".to_owned(), "BF16", vec![3]),
            (
                "layers.1.engram.embed.weight".to_owned(),
                "F8_E4M3",
                vec![3, 4],
            ),
            (
                "layers.1.engram.embed.scale".to_owned(),
                "F8_E8M0",
                vec![3, 2],
            ),
        ];
        for layer in 0..2 {
            for expert in 0..2 {
                for matrix in 1..=3 {
                    let prefix = format!("layers.{layer}.ffn.experts.{expert}.w{matrix}");
                    tensors.push((format!("{prefix}.weight"), "I8", vec![2, 3]));
                    tensors.push((format!("{prefix}.scale"), "F8_E8M0", vec![2]));
                }
            }
        }
        edit(&mut tensors);

        let mut header = serde_json::Map::new();
        let mut weight_map = serde_json::Map::new();
        let mut position = 0u64;
        for (name, dtype, shape) in &tensors {
            let element = if *dtype == "BF16" { 2 } else { 1 };
            let len = shape.iter().product::<u64>() * element;
            header.insert(
                name.clone(),
                json!({"dtype": dtype, "shape": shape, "data_offsets": [position, position + len]}),
            );
            weight_map.insert(name.clone(), json!("model-1.safetensors"));
            position += len;
        }
        header.insert("__metadata__".to_owned(), json!({"format": "pt"}));
        let header = Value::Object(header).to_string();
        let mut bytes = (header.len() as u64).to_le_bytes().to_vec();
        bytes.extend_from_slice(header.as_bytes());
        bytes.resize(bytes.len() + position as usize, 0);
        fs::write(dir.join("model-1.safetensors"), bytes).unwrap();
        fs::write(
            dir.join("model.safetensors.index.json"),
            json!({"weight_map": weight_map}).to_string(),
        )
        .unwrap();
        Fixture(dir)
    }

    fn open(fixture: &Fixture) -> Result<Catalog> {
        Catalog::open(&fixture.0)
    }

    #[test]
    fn indexes_sizes_and_extents() {
        let fixture = fixture(|_| {});
        let catalog = open(&fixture).unwrap();
        assert_eq!(
            catalog.sizes,
            Sizes {
                core: 16,
                experts: 4 * 3 * 8,
                engram: 18,
                mtp: 5,
                vision: 6
            }
        );
        assert_eq!(catalog.expert_count(), 4);
        assert_eq!(catalog.expert_slot, 24);

        let extents = catalog.expert(1, 1).unwrap();
        assert!(
            extents
                .iter()
                .all(|extent| extent.len == 6 || extent.len == 2)
        );
        assert_eq!(extents[0].len, 6);
        assert_eq!(extents[1].len, 2);
        assert!(catalog.expert(2, 0).is_none());
        assert!(catalog.expert(0, 2).is_none());
    }

    #[test]
    fn addresses_engram_rows() {
        let fixture = fixture(|_| {});
        let catalog = open(&fixture).unwrap();
        let table = catalog.engram[0];
        assert_eq!(table.layer, 1);
        let (weight, scale) = table.row(2).unwrap();
        assert_eq!((weight.offset, weight.len), (table.weight.offset + 8, 4));
        assert_eq!((scale.offset, scale.len), (table.scale.offset + 4, 2));
        assert!(table.row(3).is_none());
    }

    #[test]
    fn extents_point_at_their_tensor_bytes() {
        let fixture = fixture(|_| {});
        let catalog = open(&fixture).unwrap();
        let len = fs::metadata(&catalog.shards[0]).unwrap().len();
        // embed.weight is laid out first, right after the header; the last expert
        // scale is laid out last and must end exactly at the end of the file.
        let last = catalog.expert(1, 1).unwrap()[5];
        assert_eq!(last.offset + last.len, len);
    }

    #[test]
    fn rejects_missing_expert_weight() {
        let fixture =
            fixture(|tensors| tensors.retain(|t| t.0 != "layers.0.ffn.experts.1.w2.scale"));
        assert!(
            open(&fixture)
                .unwrap_err()
                .to_string()
                .contains("missing a weight")
        );
    }

    #[test]
    fn rejects_expert_outside_config() {
        let fixture = fixture(|tensors| {
            tensors.push(("layers.2.ffn.experts.0.w1.weight".to_owned(), "I8", vec![1]));
        });
        assert!(
            open(&fixture)
                .unwrap_err()
                .to_string()
                .contains("outside the configured model")
        );
    }

    #[test]
    fn rejects_engram_row_count_mismatch() {
        let fixture = fixture(|tensors| {
            tensors
                .iter_mut()
                .find(|t| t.0 == "layers.1.engram.embed.scale")
                .unwrap()
                .2 = vec![4, 2];
        });
        assert!(
            open(&fixture)
                .unwrap_err()
                .to_string()
                .contains("does not have 3 rows")
        );
    }

    #[test]
    fn rejects_shard_name_leaving_model_directory() {
        let fixture = fixture(|_| {});
        fs::write(
            fixture.0.join("model.safetensors.index.json"),
            json!({"weight_map": {"embed.weight": "../model-1.safetensors"}}).to_string(),
        )
        .unwrap();
        assert!(
            open(&fixture)
                .unwrap_err()
                .to_string()
                .contains("unsafe shard name")
        );
    }

    #[test]
    fn rejects_tensor_past_end_of_file() {
        let fixture = fixture(|_| {});
        let path = fixture.0.join("model-1.safetensors");
        let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(file.metadata().unwrap().len() - 1).unwrap();
        assert!(
            open(&fixture)
                .unwrap_err()
                .to_string()
                .contains("past end of file")
        );
    }

    #[test]
    fn rejects_oversized_header() {
        let fixture = fixture(|_| {});
        fs::write(
            fixture.0.join("model-1.safetensors"),
            u64::MAX.to_le_bytes(),
        )
        .unwrap();
        assert!(
            open(&fixture)
                .unwrap_err()
                .to_string()
                .contains("exceeds limit")
        );
    }

    #[test]
    fn rejects_tensors_missing_from_shards() {
        let fixture = fixture(|_| {});
        let path = fixture.0.join("model.safetensors.index.json");
        let mut index: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        index["weight_map"]["ghost"] = json!("model-1.safetensors");
        fs::write(path, index.to_string()).unwrap();
        assert!(
            open(&fixture)
                .unwrap_err()
                .to_string()
                .contains("tensors but shards hold")
        );
    }
}
