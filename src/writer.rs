use std::borrow::Cow;
use std::hash::{Hash, Hasher};

use buffa::Message as _;
use dup_indexer::{DefaultHashBuilder, DupIndexerRefs};
use indexmap::{Equivalent, IndexSet};
use usize_cast::IntoUsize;

use crate::generated::vector_tile::Tile;
use crate::generated::vector_tile::tile::{Feature, Layer, Value};
use crate::geom_writer::encode_geometry;
use crate::{DEFAULT_EXTENT, MvtError, MvtExtent, MvtGeometry, MvtResult, MvtTile, MvtValue};

#[derive(Debug, Default)]
pub struct MvtTileBuilder(Tile);

impl MvtTileBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_capacity(layers: usize) -> Self {
        Self(Tile {
            layers: Vec::with_capacity(layers),
        })
    }

    pub fn layer(self, name: impl Into<String>) -> MvtResult<MvtLayerBuilder> {
        self.layer_with_capacity(name, 0)
    }

    pub fn layer_with_capacity(
        self,
        name: impl Into<String>,
        features: usize,
    ) -> MvtResult<MvtLayerBuilder> {
        let name = name.into();
        if name.is_empty() {
            return Err(MvtError::MissingLayerName);
        }
        Ok(MvtLayerBuilder::with_tile(self, name, features))
    }

    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        self.0.encode_to_vec()
    }

    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.0.encoded_len().into_usize()
    }

    fn push_layer(mut self, layer: Layer) -> Self {
        self.0.layers.push(layer);
        self
    }
}

pub(crate) fn encode_tile(tile: MvtTile) -> MvtResult<Vec<u8>> {
    let mut tile_bld = MvtTileBuilder::with_capacity(tile.layers.len());
    for layer in tile.layers {
        let mut layer_bld = tile_bld.layer_with_capacity(layer.name, layer.features.len())?;
        layer_bld.extent(layer.extent);
        for feature in layer.features {
            let mut feature_bld =
                layer_bld.feature_with_capacity(&feature.geometry, feature.properties.len())?;
            feature_bld.id(feature.id);
            for (key, value) in feature.properties {
                feature_bld.tag(key, value)?;
            }
            layer_bld = feature_bld.end();
        }
        tile_bld = layer_bld.end();
    }
    Ok(tile_bld.encode())
}

pub(crate) fn encode_tile_ref(tile: &MvtTile) -> MvtResult<Vec<u8>> {
    let mut tile_bld = MvtTileBuilder::with_capacity(tile.layers.len());
    for layer in &tile.layers {
        let mut layer_bld =
            tile_bld.layer_with_capacity(layer.name.clone(), layer.features.len())?;
        layer_bld.extent(layer.extent);
        for feature in &layer.features {
            let mut feature_bld =
                layer_bld.feature_with_capacity(&feature.geometry, feature.properties.len())?;
            feature_bld.id(feature.id);
            for (key, value) in &feature.properties {
                feature_bld.tag_ref(key, value)?;
            }
            layer_bld = feature_bld.end();
        }
        tile_bld = layer_bld.end();
    }
    Ok(tile_bld.encode())
}

#[derive(Debug)]
pub struct MvtLayerBuilder {
    tile: MvtTileBuilder,
    layer: Layer,
    keys: DupIndexerRefs<String>,
    values: IndexSet<MvtValue, DefaultHashBuilder>,
}

impl MvtLayerBuilder {
    /// Create a standalone layer builder that is not attached to a tile.
    ///
    /// This is also a convenient entry point for building a layer directly: add
    /// features and tags as usual, then either [`end`](Self::end) it into a tile
    /// or [`encode`](Self::encode) it on its own.
    ///
    /// Finishing with [`encode`](Self::encode) yields a framed layer chunk.
    /// Independently built layer buffers (for example, one per thread) can be
    /// concatenated to form a complete tile — see the crate-level parallel
    /// encoding example. Returns [`MvtError::MissingLayerName`] if `name` is empty.
    pub fn new(name: impl Into<String>) -> MvtResult<Self> {
        MvtTileBuilder::new().layer(name)
    }

    /// Like [`MvtLayerBuilder::new`], but preallocates space for `features`.
    pub fn with_capacity(name: impl Into<String>, features: usize) -> MvtResult<Self> {
        MvtTileBuilder::new().layer_with_capacity(name, features)
    }

    fn with_tile(tile: MvtTileBuilder, name: String, features: usize) -> Self {
        Self {
            tile,
            layer: Layer {
                version: 2,
                name,
                features: Vec::with_capacity(features),
                keys: Vec::new(),
                values: Vec::new(),
                extent: Some(DEFAULT_EXTENT.get()),
            },
            keys: DupIndexerRefs::new(),
            values: IndexSet::with_hasher(DefaultHashBuilder::default()),
        }
    }

    pub fn extent(&mut self, extent: MvtExtent) -> &mut Self {
        self.layer.extent = Some(extent.get());
        self
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.layer.name
    }

    #[must_use]
    pub fn num_features(&self) -> usize {
        self.layer.features.len()
    }

    pub fn feature(self, geometry: &MvtGeometry) -> MvtResult<MvtFeatureBuilder> {
        self.feature_with_capacity(geometry, 0)
    }

    /// Start a feature with preallocated space for `tags` key-value pairs.
    pub fn feature_with_capacity(
        self,
        geometry: &MvtGeometry,
        tags: usize,
    ) -> MvtResult<MvtFeatureBuilder> {
        let (geom_type, geometry) = encode_geometry(geometry)?;
        Ok(MvtFeatureBuilder {
            layer: self,
            feature: Feature {
                id: None,
                tags: Vec::with_capacity(tags.saturating_mul(2)),
                r#type: Some(geom_type),
                geometry,
            },
        })
    }

    #[must_use]
    pub fn end(self) -> MvtTileBuilder {
        let Self {
            tile,
            mut layer,
            keys,
            values,
        } = self;
        layer.keys = keys.into_vec();
        layer.values = values.into_iter().map(value_to_proto).collect();
        tile.push_layer(layer)
    }

    /// Commit this layer and start a new one.
    ///
    /// This is a shortcut for `self.end().layer(name)` that keeps the chain on
    /// layer builders without exposing the intermediate [`MvtTileBuilder`].
    /// Returns [`MvtError::MissingLayerName`] if `name` is empty.
    pub fn layer(self, name: impl Into<String>) -> MvtResult<Self> {
        self.end().layer(name)
    }

    /// Commit this layer and encode the tile built so far.
    ///
    /// For a builder created with [`MvtLayerBuilder::new`], the parent tile is
    /// empty, so this encodes exactly this one layer as a framed chunk — several
    /// such buffers can be concatenated (for example with `buffers.concat()`)
    /// into a multi-layer tile. For a builder obtained from
    /// [`MvtTileBuilder::layer`], the result also includes any previously
    /// committed layers, making it equivalent to `self.end().encode()`.
    #[must_use]
    pub fn encode(self) -> Vec<u8> {
        self.end().encode()
    }
}

#[derive(Debug)]
#[must_use = "call .end() to commit the feature to the layer"]
pub struct MvtFeatureBuilder {
    layer: MvtLayerBuilder,
    feature: Feature,
}

impl MvtFeatureBuilder {
    pub fn id(&mut self, id: Option<u64>) -> &mut Self {
        self.feature.id = id;
        self
    }

    pub fn tag(
        &mut self,
        key: impl AsRef<str>,
        value: impl Into<MvtValue>,
    ) -> MvtResult<&mut Self> {
        let value = value.into();
        if value == MvtValue::Null {
            return Ok(self);
        }
        let index = self.layer.values.insert_full(value).0;
        self.tag_index(key.as_ref(), index)
    }

    /// Add a borrowed value, cloning it only when it is new to this layer.
    pub fn tag_ref(&mut self, key: impl AsRef<str>, value: &MvtValue) -> MvtResult<&mut Self> {
        if *value == MvtValue::Null {
            return Ok(self);
        }
        let index = self
            .layer
            .values
            .get_index_of(value)
            .unwrap_or_else(|| self.layer.values.insert_full(value.clone()).0);
        self.tag_index(key.as_ref(), index)
    }

    /// Add a string, copying its contents only when it is new to this layer.
    pub fn tag_string<'a>(
        &mut self,
        key: impl AsRef<str>,
        value: impl Into<Cow<'a, str>>,
    ) -> MvtResult<&mut Self> {
        let value = value.into();
        let index = self
            .layer
            .values
            .get_index_of(&StringValue(value.as_ref()))
            .unwrap_or_else(|| {
                self.layer
                    .values
                    .insert_full(MvtValue::String(value.into_owned()))
                    .0
            });
        self.tag_index(key.as_ref(), index)
    }

    fn tag_index(&mut self, key: &str, index: usize) -> MvtResult<&mut Self> {
        let key_idx = u32_index(self.layer.keys.insert_ref(key))?;
        let value_idx = u32_index(index)?;
        self.feature.tags.extend([key_idx, value_idx]);
        Ok(self)
    }

    pub fn tag_float(&mut self, key: impl AsRef<str>, value: f32) -> MvtResult<&mut Self> {
        self.tag(key, MvtValue::Float(value))
    }

    pub fn tag_double(&mut self, key: impl AsRef<str>, value: f64) -> MvtResult<&mut Self> {
        self.tag(key, MvtValue::Double(value))
    }

    pub fn tag_int(&mut self, key: impl AsRef<str>, value: i64) -> MvtResult<&mut Self> {
        self.tag(key, MvtValue::Int(value))
    }

    pub fn tag_uint(&mut self, key: impl AsRef<str>, value: u64) -> MvtResult<&mut Self> {
        self.tag(key, MvtValue::UInt(value))
    }

    pub fn tag_sint(&mut self, key: impl AsRef<str>, value: i64) -> MvtResult<&mut Self> {
        self.tag(key, MvtValue::SInt(value))
    }

    /// Add an integer tag using the smallest MVT encoding for `value`.
    ///
    /// See [`MvtValue::auto_int`] for how the encoding is chosen.
    pub fn tag_auto_int(
        &mut self,
        key: impl AsRef<str>,
        value: impl Into<i64>,
    ) -> MvtResult<&mut Self> {
        self.tag(key, MvtValue::auto_int(value))
    }

    pub fn tag_bool(&mut self, key: impl AsRef<str>, value: bool) -> MvtResult<&mut Self> {
        self.tag(key, MvtValue::Bool(value))
    }

    #[must_use]
    pub fn num_tags(&self) -> usize {
        self.feature.tags.len() / 2
    }

    #[must_use]
    pub fn end(mut self) -> MvtLayerBuilder {
        self.layer.layer.features.push(self.feature);
        self.layer
    }
}

// A borrowed string query with the same variant-sensitive hash as MvtValue.
struct StringValue<'a>(&'a str);

impl Hash for StringValue<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(&MvtValue::String(String::new())).hash(state);
        self.0.hash(state);
    }
}

impl Equivalent<MvtValue> for StringValue<'_> {
    fn equivalent(&self, value: &MvtValue) -> bool {
        matches!(value, MvtValue::String(string) if string == self.0)
    }
}

fn value_to_proto(value: MvtValue) -> Value {
    match value {
        MvtValue::String(v) => Value::default().with_string_value(v),
        MvtValue::Float(v) => Value::default().with_float_value(v),
        MvtValue::Double(v) => Value::default().with_double_value(v),
        MvtValue::Int(v) => Value::default().with_int_value(v),
        MvtValue::UInt(v) => Value::default().with_uint_value(v),
        MvtValue::SInt(v) => Value::default().with_sint_value(v),
        MvtValue::Bool(v) => Value::default().with_bool_value(v),
        MvtValue::Null => Value::default(),
    }
}

fn u32_index(value: usize) -> MvtResult<u32> {
    u32::try_from(value).map_err(|_| MvtError::IndexOverflow(value))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic_in_result_fn)]

    use geo_types::point;

    use super::*;
    use crate::MvtGeometry;

    #[test]
    fn layer_builder_deduplicates_keys_and_values() {
        let layer = MvtTileBuilder::new().layer("layer").unwrap();
        let mut feature = layer
            .feature(&MvtGeometry::Point(point! { x: 1, y: 2 }))
            .unwrap();
        feature.tag("foo", MvtValue::String("bar".into())).unwrap();
        feature.tag("foo", MvtValue::String("baz".into())).unwrap();
        feature.tag("bar", MvtValue::String("bar".into())).unwrap();
        feature.tag("n", MvtValue::Int(1)).unwrap();
        feature.tag("n", MvtValue::SInt(1)).unwrap();
        feature.tag("f", MvtValue::Float(f32::NAN)).unwrap();
        feature.tag("f", MvtValue::Float(f32::NAN)).unwrap();

        assert_eq!(
            feature.feature.tags,
            vec![0, 0, 0, 1, 1, 0, 2, 2, 2, 3, 3, 4, 3, 4]
        );
    }

    #[test]
    fn borrowed_tags_preserve_value_types_bits_and_insertion_order() {
        let geometry = MvtGeometry::Point(point! { x: 1, y: 2 });
        let mut owned = MvtLayerBuilder::new("values")
            .unwrap()
            .feature(&geometry)
            .unwrap();
        let mut borrowed = MvtLayerBuilder::new("values")
            .unwrap()
            .feature(&geometry)
            .unwrap();
        let values = [
            MvtValue::String(String::new()),
            MvtValue::String("road".into()),
            MvtValue::String("Łódź".into()),
            MvtValue::Int(1),
            MvtValue::SInt(1),
            MvtValue::UInt(1),
            MvtValue::Bool(true),
            MvtValue::Float(0.0),
            MvtValue::Float(-0.0),
            MvtValue::Float(f32::from_bits(0x7fc0_0001)),
            MvtValue::Float(f32::from_bits(0x7fc0_0002)),
            MvtValue::Double(0.0),
            MvtValue::Double(-0.0),
            MvtValue::Double(f64::from_bits(0x7ff8_0000_0000_0001)),
            MvtValue::Double(f64::from_bits(0x7ff8_0000_0000_0002)),
            MvtValue::Null,
        ];
        for value in &values {
            owned.tag("key", value.clone()).unwrap();
            match value {
                MvtValue::String(string) => {
                    borrowed.tag_string("key", string.as_str()).unwrap();
                }
                _ => {
                    borrowed.tag_ref("key", value).unwrap();
                }
            }
        }
        // Query values inserted through both APIs, in a different order.
        for value in values.iter().rev() {
            owned.tag("second", value.clone()).unwrap();
            borrowed.tag_ref("second", value).unwrap();
        }
        assert_eq!(borrowed.layer.values.len(), values.len() - 1);
        assert_eq!(owned.end().encode(), borrowed.end().encode());
    }

    #[test]
    fn encode_appends_and_validates_tile_metadata() {
        let tile = MvtTileBuilder::new();
        let layer = tile.layer("layer").unwrap();
        let mut feature = layer
            .feature(&MvtGeometry::Point(point! { x: 1, y: 2 }))
            .unwrap();
        feature.id(Some(1));
        feature.tag("skip", MvtValue::Null).unwrap();
        let layer = feature.end();
        let bytes = layer.end().encode();
        let proto = Tile::decode_from_slice(&bytes).unwrap();
        assert!(proto.layers[0].keys.is_empty());
        assert!(proto.layers[0].features[0].tags.is_empty());

        let tile = MvtTileBuilder::new();
        let layer = tile.layer("layer").unwrap();
        let mut feature = layer
            .feature(&MvtGeometry::Point(point! { x: 1, y: 2 }))
            .unwrap();
        feature.id(Some(1));
        let layer = feature.end();
        let tile = layer.end();
        let mut out = vec![0xaa];
        out.extend_from_slice(&tile.encode());
        assert_eq!(out[0], 0xaa);

        let tile = MvtTileBuilder::new();
        let tile = tile.layer("same").unwrap().end();
        let tile = tile.layer("same").unwrap().end();
        assert!(!tile.encode().is_empty());
    }

    #[test]
    fn encode_ref_matches_owned_encode() {
        let mut feature = crate::MvtFeature::new(MvtGeometry::Point(point! { x: 1, y: 2 }));
        feature.set_id(7);
        feature.add_tag_string("name", "Example");
        feature.add_tag_bool("visible", true);

        let mut layer = crate::MvtLayer::new("places", DEFAULT_EXTENT);
        layer.add_feature(feature);

        let mut tile = MvtTile::new();
        tile.add_layer(layer);

        assert_eq!(
            encode_tile(tile.clone()).unwrap(),
            encode_tile_ref(&tile).unwrap()
        );
    }

    #[test]
    #[cfg(feature = "reader")]
    fn standalone_layer_encode_matches_tile_path_and_concatenates() {
        use crate::reader::MvtReaderRef;

        let build = |name| -> MvtResult<Vec<u8>> {
            let mut feature =
                MvtLayerBuilder::new(name)?.feature(&MvtGeometry::Point(point! { x: 1, y: 2 }))?;
            feature.tag("k", MvtValue::UInt(1))?;
            Ok(feature.end().encode())
        };

        // A standalone layer buffer equals the same layer built via the tile path.
        let via_tile = MvtTileBuilder::new()
            .layer("roads")
            .unwrap()
            .feature(&MvtGeometry::Point(point! { x: 1, y: 2 }))
            .unwrap();
        let mut via_tile = via_tile;
        via_tile.tag("k", MvtValue::UInt(1)).unwrap();
        let via_tile = via_tile.end().end().encode();
        assert_eq!(build("roads").unwrap(), via_tile);

        // Concatenated layer buffers form a valid multi-layer tile.
        let tile = [build("roads").unwrap(), build("water").unwrap()].concat();
        let reader = MvtReaderRef::new(&tile).unwrap();
        let names: Vec<_> = reader.layers().map(|l| l.name().to_string()).collect();
        assert_eq!(names, ["roads", "water"]);
    }

    #[test]
    #[cfg(feature = "reader")]
    fn layer_builder_chains_to_next_layer() -> MvtResult<()> {
        use crate::reader::MvtReaderRef;

        // Chaining `.layer(..)` keeps the builder on the layer without exposing
        // the tile, and produces the same tile as the explicit tile path.
        let chained = MvtLayerBuilder::new("roads")?
            .feature(&MvtGeometry::Point(point! { x: 1, y: 2 }))?
            .end()
            .layer("water")?
            .feature(&MvtGeometry::Point(point! { x: 3, y: 4 }))?
            .end()
            .encode();

        let reader = MvtReaderRef::new(&chained)?;
        let names: Vec<_> = reader.layers().map(|l| l.name().to_string()).collect();
        assert_eq!(names, ["roads", "water"]);
        Ok(())
    }

    #[test]
    fn layer_builder_chain_rejects_empty_name() {
        let layer = MvtLayerBuilder::new("roads").unwrap();
        assert!(matches!(layer.layer(""), Err(MvtError::MissingLayerName)));
    }

    #[test]
    fn standalone_layer_builder_rejects_empty_name() {
        assert!(matches!(
            MvtLayerBuilder::new(""),
            Err(MvtError::MissingLayerName)
        ));
        assert!(matches!(
            MvtLayerBuilder::with_capacity("", 1),
            Err(MvtError::MissingLayerName)
        ));
    }

    #[test]
    fn layer_builder_rejects_empty_name() {
        assert!(matches!(
            MvtTileBuilder::new().layer(""),
            Err(MvtError::MissingLayerName)
        ));
        assert!(matches!(
            MvtTileBuilder::new().layer_with_capacity("", 1),
            Err(MvtError::MissingLayerName)
        ));
    }

    #[test]
    fn builder_encoded_len_matches_encoded_bytes() {
        let builder = MvtTileBuilder::new()
            .layer("l")
            .unwrap()
            .feature(&MvtGeometry::Point(point! { x: 1, y: 2 }))
            .unwrap()
            .end()
            .end();
        let len = builder.encoded_len();
        assert_eq!(len, builder.encode().len());
    }

    #[test]
    fn layer_builder_accepts_feature_capacity() {
        let layer = MvtTileBuilder::new()
            .layer_with_capacity("layer", 2)
            .unwrap();
        assert_eq!(layer.layer.features.capacity(), 2);
    }

    #[test]
    #[cfg(feature = "reader")]
    fn tag_auto_int_round_trips_through_reader() {
        use crate::reader::{MvtReaderRef, MvtValueRef};

        let tile = MvtTileBuilder::new();
        let layer = tile.layer("l").unwrap();
        let mut feature = layer
            .feature(&MvtGeometry::Point(point! { x: 1, y: 2 }))
            .unwrap();
        feature.tag_auto_int("pos", 100_i32).unwrap();
        feature.tag_auto_int("neg", -100_i16).unwrap();
        feature.tag_auto_int("zero", 0_i64).unwrap();
        let bytes = feature.end().end().encode();

        let reader = MvtReaderRef::new(&bytes).unwrap();
        let layer = reader.layers().next().unwrap();
        let feature = layer.features().next().unwrap();
        let props = feature.properties_vec().unwrap();

        // Non-negative -> UInt, negative -> SInt.
        assert_eq!(props[0].0, "pos");
        assert_eq!(props[0].1, MvtValueRef::UInt(100));
        assert_eq!(props[1].0, "neg");
        assert_eq!(props[1].1, MvtValueRef::SInt(-100));
        assert_eq!(props[2].0, "zero");
        assert_eq!(props[2].1, MvtValueRef::UInt(0));
    }

    #[test]
    fn auto_int_is_never_larger_than_int_or_sint() {
        for v in [
            0_i64,
            1,
            63,
            64,
            127,
            128,
            -1,
            -64,
            -100,
            i64::MIN,
            i64::MAX,
        ] {
            let auto = value_to_proto(MvtValue::auto_int(v)).encoded_len();
            let int = value_to_proto(MvtValue::Int(v)).encoded_len();
            let sint = value_to_proto(MvtValue::SInt(v)).encoded_len();
            assert!(auto <= int, "v={v}: auto {auto} > int {int}");
            assert!(auto <= sint, "v={v}: auto {auto} > sint {sint}");
        }
    }

    #[test]
    fn value_to_proto_handles_all_variants() {
        assert_eq!(
            value_to_proto(MvtValue::String("x".into()))
                .string_value
                .as_deref(),
            Some("x")
        );
        assert_eq!(value_to_proto(MvtValue::Float(1.0)).float_value, Some(1.0));
        assert_eq!(
            value_to_proto(MvtValue::Double(2.0)).double_value,
            Some(2.0)
        );
        assert_eq!(value_to_proto(MvtValue::Int(-3)).int_value, Some(-3));
        assert_eq!(value_to_proto(MvtValue::UInt(4)).uint_value, Some(4));
        assert_eq!(value_to_proto(MvtValue::SInt(-5)).sint_value, Some(-5));
        assert_eq!(value_to_proto(MvtValue::Bool(true)).bool_value, Some(true));
        assert_eq!(value_to_proto(MvtValue::Null), Value::default());
    }
}
