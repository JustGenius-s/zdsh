use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, LazyLock, OnceLock},
};

use anyhow::{Context as _, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use crate::geometry::IconNode;

const PAYLOAD: &[u8] = include_bytes!("../assets/glyphs.bin");

macro_rules! icon_names {
    ($($variant:ident => $name:literal,)*) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum IconName {
            $($variant,)*
        }

        impl IconName {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            pub fn path(self) -> gpui::SharedString {
                self.path_with_variant(Variant::Outline)
            }

            pub fn path_with_variant(self, variant: Variant) -> gpui::SharedString {
                match variant {
                    Variant::Outline => format!("gpui-icons/{}.svg", self.as_str()).into(),
                    Variant::Filled => format!("gpui-icons/{}-filled.svg", self.as_str()).into(),
                }
            }
        }

        impl gpui_kit::assets::IconNamed for IconName {
            fn path(self) -> gpui::SharedString {
                self.path()
            }
        }
    };
}

include!("../assets/icon_names.rs");

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Variant {
    #[default]
    Outline,
    Filled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GlyphSource {
    Cursor,
    Drawn,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct IconReference {
    pub name: Arc<str>,
    pub variant: Variant,
    pub source: GlyphSource,
    pub mirrored_in_rtl: bool,
}

#[derive(Debug)]
pub struct Glyph {
    pub id: u32,
    pub source: GlyphSource,
    pub glyph_name: Arc<str>,
    pub codepoint: u32,
    pub em_units: u16,
    pub advance: f64,
    pub advance_units: u16,
    pub baseline: f64,
    pub left_side_bearing: f64,
    pub left_side_bearing_units: i16,
    pub view_box: [f64; 4],
    pub nodes: Vec<IconNode>,
    pub svg: Arc<str>,
}

#[derive(Debug, Deserialize)]
pub struct IconEntry {
    pub name: String,
    pub outline: usize,
    pub filled: usize,
    pub has_explicit_filled: bool,
    pub mirrored_in_rtl: bool,
}

#[derive(Debug, Deserialize)]
pub struct DrawnEntry {
    pub name: String,
    pub glyph: usize,
    pub paths: Vec<String>,
    pub derived_from: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CatalogMetadata {
    pub version: u32,
    pub coordinate_size: f64,
    pub em_units: u16,
    pub baseline: f64,
    pub ascent: i16,
    pub descent: i16,
    pub line_gap: i16,
    pub font_sha256: String,
    pub display_view_box: [f64; 4],
}

#[derive(Deserialize)]
struct CatalogData {
    #[serde(flatten)]
    metadata: CatalogMetadata,
    entries: Vec<IconEntry>,
    drawn_entries: Vec<DrawnEntry>,
    glyphs: Vec<GlyphRecord>,
}

#[derive(Deserialize)]
struct GlyphRecord {
    id: u32,
    source: GlyphSource,
    codepoint: u32,
    glyph_name: String,
    em_units: u16,
    baseline: f64,
    advance: f64,
    advance_units: u16,
    left_side_bearing: f64,
    left_side_bearing_units: i16,
    svg: DataRange,
    view_box: [f64; 4],
    nodes: DataRange,
}

#[derive(Deserialize)]
struct DataRange {
    offset: usize,
    length: usize,
}

impl DataRange {
    fn bytes(&self) -> Result<&'static [u8]> {
        let end = self
            .offset
            .checked_add(self.length)
            .context("Icon payload range overflows")?;
        PAYLOAD
            .get(self.offset..end)
            .context("Icon payload range is out of bounds")
    }
}

#[derive(Deserialize)]
struct MappedIcon {
    name: String,
    variant: Variant,
}

#[derive(Deserialize)]
struct Mappings {
    legacy: HashMap<String, MappedIcon>,
    kit: HashMap<String, MappedIcon>,
}

struct Catalog {
    data: CatalogData,
    names: HashMap<String, usize>,
    drawn_names: HashMap<String, usize>,
    drawn_paths: HashMap<String, usize>,
    mappings: Mappings,
    glyphs: Vec<OnceLock<Result<Arc<Glyph>, String>>>,
}

static CATALOG: LazyLock<Result<Catalog, String>> =
    LazyLock::new(|| Catalog::decode().map_err(|error| format!("{error:#}")));

impl Catalog {
    fn decode() -> Result<Self> {
        let data: CatalogData = serde_json::from_str(include_str!("../assets/catalog.json"))
            .context("Decoding the embedded icon catalog")?;
        ensure!(
            data.metadata.version == 4,
            "Unsupported icon catalog version"
        );
        let mappings: Mappings = serde_json::from_str(include_str!("../assets/mappings.json"))
            .context("Decoding icon compatibility mappings")?;
        let mut names = HashMap::with_capacity(data.entries.len());
        for (index, entry) in data.entries.iter().enumerate() {
            ensure!(
                names.insert(entry.name.clone(), index).is_none(),
                "Duplicate icon name {:?}",
                entry.name
            );
            ensure!(
                data.glyphs.get(entry.outline).is_some() && data.glyphs.get(entry.filled).is_some(),
                "Invalid glyph reference for {:?}",
                entry.name
            );
        }
        for mapping in mappings.legacy.values().chain(mappings.kit.values()) {
            ensure!(
                names.contains_key(&mapping.name),
                "Mapping refers to unknown icon {:?}",
                mapping.name
            );
        }
        let mut drawn_names = HashMap::with_capacity(data.drawn_entries.len());
        let mut drawn_paths = HashMap::new();
        for (index, entry) in data.drawn_entries.iter().enumerate() {
            ensure!(
                !names.contains_key(&entry.name)
                    && drawn_names.insert(entry.name.clone(), index).is_none(),
                "Duplicate drawn icon name {:?}",
                entry.name
            );
            let record = data
                .glyphs
                .get(entry.glyph)
                .context("Invalid drawn glyph reference")?;
            ensure!(
                record.source == GlyphSource::Drawn,
                "Drawn entry refers to a Cursor glyph"
            );
            for path in &entry.paths {
                ensure!(
                    drawn_paths.insert(path.clone(), index).is_none(),
                    "Duplicate drawn asset path {path:?}"
                );
            }
        }
        let mut glyph_ids = std::collections::HashSet::with_capacity(data.glyphs.len());
        for record in &data.glyphs {
            ensure!(
                glyph_ids.insert(record.id),
                "Duplicate glyph cache identity"
            );
        }
        let glyphs = (0..data.glyphs.len()).map(|_| OnceLock::new()).collect();
        Ok(Self {
            data,
            names,
            drawn_names,
            drawn_paths,
            mappings,
            glyphs,
        })
    }

    fn entry(&self, name: &str) -> Option<&IconEntry> {
        self.names
            .get(name)
            .and_then(|index| self.data.entries.get(*index))
    }

    fn reference(&self, name: &str, variant: Variant) -> Option<IconReference> {
        if let Some(entry) = self.entry(name) {
            Some(IconReference {
                name: entry.name.as_str().into(),
                variant,
                source: GlyphSource::Cursor,
                mirrored_in_rtl: entry.mirrored_in_rtl,
            })
        } else {
            let index = self.drawn_names.get(name)?;
            let entry = self.data.drawn_entries.get(*index)?;
            Some(IconReference {
                name: entry.name.as_str().into(),
                variant: Variant::Outline,
                source: GlyphSource::Drawn,
                mirrored_in_rtl: false,
            })
        }
    }

    fn drawn_reference(&self, path: &str) -> Option<IconReference> {
        let index = self.drawn_paths.get(path)?;
        let entry = self.data.drawn_entries.get(*index)?;
        self.reference(&entry.name, Variant::Outline)
    }

    fn resolve(&self, path: &str) -> Option<IconReference> {
        if let Some(name) = path
            .strip_prefix("gpui-icons/")
            .and_then(|name| name.strip_suffix(".svg"))
        {
            if let Some(name) = name.strip_suffix("-filled")
                && let Some(reference) = self.reference(name, Variant::Filled)
            {
                return Some(reference);
            }
            return self.reference(name, Variant::Outline);
        }
        let mapping = if let Some(name) = path
            .strip_prefix("kit/icons/")
            .and_then(|name| name.strip_suffix(".svg"))
        {
            self.mappings.kit.get(name)
        } else if let Some(name) = path
            .strip_prefix("icons/")
            .and_then(|name| name.strip_suffix(".svg"))
        {
            // Unprefixed component paths share the old asset source; its names win collisions.
            if let Some(mapping) = self.mappings.legacy.get(name) {
                return self.reference(&mapping.name, mapping.variant);
            }
            if let Some(reference) = self.drawn_reference(path) {
                return Some(reference);
            }
            self.mappings.kit.get(name)
        } else {
            None
        };
        mapping
            .and_then(|mapping| self.reference(&mapping.name, mapping.variant))
            .or_else(|| self.drawn_reference(path))
    }

    fn glyph_index(&self, name: &str, variant: Variant) -> Option<usize> {
        if let Some(entry) = self.entry(name) {
            Some(match variant {
                Variant::Outline => entry.outline,
                Variant::Filled => entry.filled,
            })
        } else {
            self.drawn_names
                .get(name)
                .and_then(|index| self.data.drawn_entries.get(*index))
                .map(|entry| entry.glyph)
        }
    }

    fn glyph(&self, index: usize) -> Result<Arc<Glyph>> {
        let cached = self
            .glyphs
            .get(index)
            .context("Missing glyph cache entry")?;
        cached
            .get_or_init(|| {
                self.decode_glyph(index)
                    .map(Arc::new)
                    .map_err(|error| format!("{error:#}"))
            })
            .as_ref()
            .map(Arc::clone)
            .map_err(|error| anyhow!("{error}"))
    }

    fn decode_glyph(&self, index: usize) -> Result<Glyph> {
        let record = self
            .data
            .glyphs
            .get(index)
            .context("Missing glyph record")?;
        Ok(Glyph {
            id: record.id,
            source: record.source,
            glyph_name: record.glyph_name.as_str().into(),
            codepoint: record.codepoint,
            em_units: record.em_units,
            advance: record.advance,
            advance_units: record.advance_units,
            baseline: record.baseline,
            left_side_bearing: record.left_side_bearing,
            left_side_bearing_units: record.left_side_bearing_units,
            view_box: record.view_box,
            nodes: serde_json::from_slice(record.nodes.bytes()?)
                .context("Decoding embedded icon nodes")?,
            svg: std::str::from_utf8(record.svg.bytes()?)
                .context("Embedded icon SVG is not UTF-8")?
                .into(),
        })
    }
}

fn catalog() -> Result<&'static Catalog> {
    CATALOG.as_ref().map_err(|error| anyhow!("{error}"))
}

pub fn metadata() -> Result<&'static CatalogMetadata> {
    Ok(&catalog()?.data.metadata)
}

pub fn entries() -> Result<&'static [IconEntry]> {
    Ok(&catalog()?.data.entries)
}

pub fn drawn_entries() -> Result<&'static [DrawnEntry]> {
    Ok(&catalog()?.data.drawn_entries)
}

pub fn resolve_path(path: &str) -> Result<Option<IconReference>> {
    Ok(catalog()?.resolve(path))
}

pub fn glyph(name: &str, variant: Variant) -> Result<Option<Arc<Glyph>>> {
    let catalog = catalog()?;
    catalog
        .glyph_index(name, variant)
        .map(|index| catalog.glyph(index))
        .transpose()
}

pub fn glyph_for_path(path: &str) -> Result<Option<Arc<Glyph>>> {
    let catalog = catalog()?;
    let Some(reference) = catalog.resolve(path) else {
        return Ok(None);
    };
    catalog
        .glyph_index(&reference.name, reference.variant)
        .map(|index| catalog.glyph(index))
        .transpose()
}

pub fn load(path: &str) -> Result<Option<Cow<'static, [u8]>>> {
    let catalog = catalog()?;
    let Some(reference) = catalog.resolve(path) else {
        return Ok(None);
    };
    let index = catalog
        .glyph_index(&reference.name, reference.variant)
        .context("Resolved icon has no glyph")?;
    let record = catalog
        .data
        .glyphs
        .get(index)
        .context("Missing glyph record")?;
    Ok(Some(Cow::Borrowed(record.svg.bytes()?)))
}
