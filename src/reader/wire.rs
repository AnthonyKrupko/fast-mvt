//! Reader views with borrowed geometry commands. Buffa's generated scalar views
//! use Vec even for packed fields, so these three containers use its wire APIs
//! directly. Property values continue to use the generated borrowed view.

use std::cell::Cell;

use buffa::encoding::{Tag, WireType, check_wire_type, skip_field_depth};
use buffa::{DecodeContext, DecodeError, Enumeration as _, types};

use super::capacity::LayerCapacity;
use super::commands::GeometryFields;
use super::property::ValueEntry;
use crate::generated::vector_tile::{Tile, tile as proto};

#[derive(Clone, Default)]
pub(super) struct TileView<'a> {
    pub layers: Vec<LayerView<'a>>,
}

#[derive(Clone, Default)]
pub(super) struct LayerView<'a> {
    pub version: u32,
    pub name: &'a str,
    pub features: Vec<FeatureView<'a>>,
    pub tags: Vec<u32>,
    pub keys: Vec<&'a str>,
    pub values: Vec<ValueEntry<'a>>,
    pub extent: Option<u32>,
}

#[derive(Clone, Default)]
pub(super) struct FeatureView<'a> {
    pub id: Option<u64>,
    pub tags: std::ops::Range<usize>,
    pub r#type: Option<proto::GeomType>,
    pub geometry: GeometryFields<'a>,
}

impl<'a> TileView<'a> {
    pub fn decode(mut cur: &'a [u8]) -> Result<Self, DecodeError> {
        let limit = Cell::new(buffa::DEFAULT_UNKNOWN_FIELD_LIMIT);
        let ctx = DecodeContext::new(buffa::RECURSION_LIMIT, &limit);
        let mut tile = Self::default();
        while !cur.is_empty() {
            let tag = Tag::decode(&mut cur)?;
            if tag.field_number() == 3 {
                check_wire_type(tag, WireType::LengthDelimited)?;
                let subctx = ctx.descend()?;
                let sub = types::borrow_bytes(&mut cur)?;
                tile.layers.push(LayerView::decode(sub, subctx)?);
            } else {
                skip_field_depth(tag, &mut cur, ctx.depth())?;
            }
        }
        Ok(tile)
    }

    pub fn to_proto(&self) -> Result<Tile, DecodeError> {
        let mut layers = Vec::with_capacity(self.layers.len());
        for layer in &self.layers {
            layers.push(proto::Layer {
                version: layer.version,
                name: layer.name.to_string(),
                features: layer
                    .features
                    .iter()
                    .map(|feature| proto::Feature {
                        id: feature.id,
                        tags: layer.tags[feature.tags.clone()].to_vec(),
                        r#type: feature.r#type,
                        geometry: feature.geometry.iter().collect(),
                    })
                    .collect(),
                keys: layer.keys.iter().map(|key| (*key).to_string()).collect(),
                values: layer
                    .values
                    .iter()
                    .map(ValueEntry::to_proto)
                    .collect::<Result<_, _>>()?,
                extent: layer.extent,
            });
        }
        Ok(Tile { layers })
    }
}

impl<'a> LayerView<'a> {
    fn decode(mut cur: &'a [u8], ctx: DecodeContext<'_>) -> Result<Self, DecodeError> {
        let capacity = LayerCapacity::scan(cur, ctx)?;
        let mut layer = Self {
            features: Vec::with_capacity(capacity.features),
            keys: Vec::with_capacity(capacity.keys),
            values: Vec::with_capacity(capacity.values),
            tags: Vec::with_capacity(capacity.tags),
            ..Self::default()
        };
        while !cur.is_empty() {
            let tag = Tag::decode(&mut cur)?;
            match tag.field_number() {
                15 => {
                    check_wire_type(tag, WireType::Varint)?;
                    layer.version = types::decode_uint32(&mut cur)?;
                }
                1 => {
                    check_wire_type(tag, WireType::LengthDelimited)?;
                    layer.name = types::borrow_str(&mut cur)?;
                }
                5 => {
                    check_wire_type(tag, WireType::Varint)?;
                    layer.extent = Some(types::decode_uint32(&mut cur)?);
                }
                2 => {
                    check_wire_type(tag, WireType::LengthDelimited)?;
                    let subctx = ctx.descend()?;
                    let sub = types::borrow_bytes(&mut cur)?;
                    layer
                        .features
                        .push(FeatureView::decode(sub, subctx, &mut layer.tags)?);
                }
                3 => {
                    check_wire_type(tag, WireType::LengthDelimited)?;
                    layer.keys.push(types::borrow_str(&mut cur)?);
                }
                4 => {
                    check_wire_type(tag, WireType::LengthDelimited)?;
                    let subctx = ctx.descend()?;
                    let sub = types::borrow_bytes(&mut cur)?;
                    layer.values.push(ValueEntry::decode(sub, subctx)?);
                }
                _ => skip_field_depth(tag, &mut cur, ctx.depth())?,
            }
        }
        Ok(layer)
    }
}

impl<'a> FeatureView<'a> {
    fn decode(
        mut cur: &'a [u8],
        ctx: DecodeContext<'_>,
        tags: &mut Vec<u32>,
    ) -> Result<Self, DecodeError> {
        let mut feature = Self {
            tags: tags.len()..tags.len(),
            ..Self::default()
        };
        while !cur.is_empty() {
            let before_tag = cur;
            let tag = Tag::decode(&mut cur)?;
            match tag.field_number() {
                1 => {
                    check_wire_type(tag, WireType::Varint)?;
                    feature.id = Some(types::decode_uint64(&mut cur)?);
                }
                2 => match tag.wire_type() {
                    WireType::LengthDelimited => {
                        let packed = types::borrow_bytes(&mut cur)?;
                        // The layer scan already counted and reserved every tag.
                        types::extend_packed_uint32(packed, tags, 0)?;
                    }
                    WireType::Varint => tags.push(types::decode_uint32(&mut cur)?),
                    _ => {
                        return Err(buffa::encoding::wire_type_mismatch(
                            tag,
                            WireType::LengthDelimited,
                        ));
                    }
                },
                3 => {
                    check_wire_type(tag, WireType::Varint)?;
                    if let Some(value) = proto::GeomType::from_i32(types::decode_int32(&mut cur)?) {
                        feature.r#type = Some(value);
                    }
                }
                4 => feature.geometry.read(tag, before_tag, &mut cur)?,
                _ => skip_field_depth(tag, &mut cur, ctx.depth())?,
            }
        }
        feature.tags.end = tags.len();
        Ok(feature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MvtValueRef;
    use crate::generated::vector_tile::TileView as GeneratedTileView;
    use buffa::MessageView as _;

    fn bytes_field(tag: u8, value: &[u8]) -> Vec<u8> {
        let mut bytes = vec![tag];
        buffa::encoding::encode_varint(value.len() as u64, &mut bytes);
        bytes.extend_from_slice(value);
        bytes
    }

    fn tile_bytes(feature: &[u8]) -> Vec<u8> {
        let mut layer = vec![120, 2, 10, 1, b'x'];
        layer.extend(bytes_field(18, feature));
        bytes_field(26, &layer)
    }

    fn compare(bytes: &[u8]) {
        let actual = TileView::decode(bytes).and_then(|v| v.to_proto());
        let expected = GeneratedTileView::decode_view(bytes).and_then(|v| v.to_owned_message());
        match (actual, expected) {
            (Ok(actual), Ok(expected)) => assert_eq!(actual, expected, "{bytes:?}"),
            // The structural sizing pass can encounter a different fault first
            // in messages containing multiple errors. Acceptance must still
            // match the generated decoder, and successful contents match above.
            (Err(_), Err(_)) => {}
            (actual, expected) => {
                panic!("wire readers disagree: {actual:?} != {expected:?}; {bytes:?}")
            }
        }
    }

    #[test]
    fn compact_values_preserve_all_proto_fields() {
        use buffa::Message as _;
        let original = Tile {
            layers: vec![proto::Layer {
                version: 2,
                name: "x".into(),
                keys: vec!["k".into()],
                values: vec![proto::Value {
                    string_value: Some("text".into()),
                    uint_value: Some(9),
                    bool_value: Some(true),
                    ..Default::default()
                }],
                features: vec![proto::Feature {
                    tags: vec![0, 0],
                    r#type: Some(proto::GeomType::Point),
                    geometry: vec![9, 0, 0],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        let bytes = original.encode_to_vec();
        let reader = crate::MvtReaderRef::new(&bytes).unwrap();
        assert_eq!(reader.to_proto().unwrap(), original);
        let layer = reader.layers().next().unwrap();
        assert_eq!(layer.values().next().unwrap(), MvtValueRef::String("text"));
        assert_eq!(
            layer
                .features()
                .next()
                .unwrap()
                .properties()
                .next()
                .unwrap()
                .unwrap()
                .1,
            MvtValueRef::String("text")
        );
        assert!(size_of::<ValueEntry>() < size_of::<proto::ValueView>());
    }

    #[test]
    fn shared_tags_keep_feature_and_layer_ranges_separate() {
        use buffa::Message as _;
        let mut layer = vec![120, 2, 10, 1, b'x'];
        layer.extend(bytes_field(18, &[8, 7, 16, 0, 18, 1, 0, 18, 0]));
        layer.extend(bytes_field(18, &[]));
        layer.extend(bytes_field(18, &[18, 2, 1, 1, 16, 0, 16, 0]));
        // Nested field 2 in an unknown group is not a layer feature.
        layer.extend([51, 18, 0, 52]);
        for key in [b"a", b"b"] {
            layer.extend(bytes_field(26, key));
        }
        for value in ["first", "second"] {
            layer.extend(bytes_field(
                34,
                &proto::Value {
                    string_value: Some(value.into()),
                    ..Default::default()
                }
                .encode_to_vec(),
            ));
        }
        let mut bytes = bytes_field(26, &layer);
        bytes.extend(bytes_field(26, &layer));
        compare(&bytes);
        let original = crate::MvtReaderRef::new(&bytes).unwrap();
        let cloned = original.clone();
        drop(original);
        for layer in cloned.layers() {
            let features: Vec<_> = layer.features().collect();
            assert_eq!(features[0].tags(), [0, 0]);
            assert!(features[1].tags().is_empty());
            assert_eq!(features[2].tags(), [1, 1, 0, 0]);
            assert_eq!(
                features[0].properties_vec().unwrap(),
                [("a", MvtValueRef::String("first"))]
            );
            assert!(!features[1].has_properties());
            assert_eq!(
                features[2].properties_vec().unwrap(),
                [
                    ("b", MvtValueRef::String("second")),
                    ("a", MvtValueRef::String("first"))
                ]
            );
        }
        let view = TileView::decode(&bytes).unwrap();
        for layer in &view.layers {
            assert_eq!(layer.features.capacity(), 3);
            assert_eq!(layer.tags.capacity(), 6);
            assert_eq!(layer.keys.capacity(), 2);
            assert_eq!(layer.values.capacity(), 2);
        }
    }

    #[test]
    fn split_geometry_fields_preserve_order_and_iterator_length() {
        // Packed and unpacked values, an empty packed field, interleaved id,
        // and an unknown group whose field 4 must not become geometry.
        let bytes = tile_bytes(&[
            24, 1, 34, 0, 32, 9, 8, 7, 34, 1, 4, 51, 32, 99, 52, 34, 1, 6, 34, 0, 24, 99,
        ]);
        compare(&bytes);
        let reader = crate::MvtReaderRef::new(&bytes).unwrap();
        let feature = reader.layers().next().unwrap().features().next().unwrap();
        let mut commands = feature.geometry_commands();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands.next(), Some(9));
        assert_eq!(commands.size_hint(), (2, Some(2)));
        assert_eq!(commands.clone().collect::<Vec<_>>(), [4, 6]);
        assert_eq!(commands.next(), Some(4));
        assert_eq!(commands.next(), Some(6));
        assert_eq!(commands.len(), 0);
        assert_eq!(commands.next(), None);
        assert_eq!(commands.next(), None);
        assert_eq!(feature.id(), Some(7));
        assert_eq!(feature.geom_type(), Some(proto::GeomType::Point));
        assert_eq!(
            feature.geometry().unwrap(),
            geo_types::Point::new(2, 3).into()
        );
        assert_eq!(
            reader.clone().to_proto().unwrap(),
            reader.to_proto().unwrap()
        );
    }

    #[test]
    fn split_fields_decode_the_same_lines_and_rings() {
        for (kind, commands) in [
            (2, vec![17, 0, 0, 256, 128, 18, 2, 4, 1, 3, 9, 0, 0]),
            (
                3,
                vec![
                    9, 0, 0, 26, 20, 0, 0, 20, 19, 0, 15, 9, 4, 4, 18, 0, 4, 4, 0, 15,
                ],
            ),
        ] {
            let encode = |values: &[u32]| {
                let mut bytes = Vec::new();
                for &value in values {
                    types::encode_uint32(value, &mut bytes);
                }
                bytes
            };
            let mut contiguous = vec![24, kind];
            contiguous.extend(bytes_field(34, &encode(&commands)));
            let bytes = tile_bytes(&contiguous);
            let reader = crate::MvtReaderRef::new(&bytes).unwrap();
            let expected = reader
                .layers()
                .next()
                .unwrap()
                .features()
                .next()
                .unwrap()
                .geometry()
                .unwrap();
            for split in 0..=commands.len() {
                let mut feature = vec![24, kind];
                feature.extend(bytes_field(34, &encode(&commands[..split])));
                feature.extend([48, 7]);
                for &value in &commands[split..] {
                    feature.push(32);
                    types::encode_uint32(value, &mut feature);
                    feature.extend([51, 32, 99, 52]);
                }
                let bytes = tile_bytes(&feature);
                compare(&bytes);
                let reader = crate::MvtReaderRef::new(&bytes).unwrap();
                let feature = reader.layers().next().unwrap().features().next().unwrap();
                assert_eq!(feature.geometry_commands().collect::<Vec<_>>(), commands);
                assert_eq!(feature.geometry().unwrap(), expected);
            }
        }
    }

    #[test]
    fn wide_and_noncanonical_varints_match_generated_reader() {
        for value in [0, 127, 128, 16384, u64::from(u32::MAX), u64::MAX] {
            let mut encoded = Vec::new();
            buffa::encoding::encode_varint(value, &mut encoded);
            compare(&tile_bytes(&bytes_field(34, &encoded)));
            let mut unpacked = vec![32];
            unpacked.extend(encoded);
            compare(&tile_bytes(&unpacked));
        }
        compare(&tile_bytes(&[34, 3, 0x80, 0x80, 0]));
    }

    #[test]
    fn malformed_geometry_is_rejected_before_geometry_access() {
        for field in [
            vec![34, 1, 0x80],                       // truncated varint
            vec![34, 2, 0],                          // truncated packed payload
            [vec![34, 10], vec![0xff; 10]].concat(), // overflow
            [vec![34, 10], vec![0xff; 9], vec![2]].concat(),
            vec![32, 0x80],       // truncated unpacked varint
            vec![37, 0, 0, 0, 0], // wrong wire type
        ] {
            let bytes = tile_bytes(&field);
            compare(&bytes);
            assert!(crate::MvtReaderRef::new(&bytes).is_err());
        }
    }

    #[test]
    fn truncation_mutation_and_unknown_groups_match_generated_reader() {
        let mut feature = vec![8, 7, 18, 2, 0, 0, 24, 1, 34, 3, 9, 4, 6];
        // Unknown fixed32, fixed64, bytes and group fields.
        feature.extend([
            53, 0, 0, 0, 0, 57, 0, 0, 0, 0, 0, 0, 0, 0, 66, 2, 8, 0, 75, 8, 1, 76,
        ]);
        let bytes = tile_bytes(&feature);
        for end in 0..=bytes.len() {
            compare(&bytes[..end]);
        }
        for index in 0..bytes.len() {
            for replacement in 0..=u8::MAX {
                let mut changed = bytes.clone();
                changed[index] = replacement;
                compare(&changed);
            }
        }
        for depth in [0, 1, 95, 98, 99, 100, 101] {
            let mut groups = vec![51; depth];
            groups.extend([32, 9]);
            groups.extend(vec![52; depth]);
            compare(&tile_bytes(&groups));
        }
    }

    #[test]
    fn all_fixtures_match_generated_proto_reader() {
        fn visit(path: &std::path::Path) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(&path);
                } else if path.extension().is_some_and(|ext| ext == "mvt") {
                    compare(&std::fs::read(path).unwrap());
                }
            }
        }
        visit(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/mvt-fixtures"));
    }
}
