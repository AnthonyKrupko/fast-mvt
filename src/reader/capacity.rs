use buffa::encoding::{Tag, WireType, check_wire_type, count_varints, skip_field_depth};
use buffa::{DecodeContext, DecodeError, types};

/// Count storage directly from bounded wire fields before allocating a layer.
/// Counts cannot exceed the number of bytes in the layer: each element must
/// occupy an actual tag or varint in a bounds-checked field. The decode pass
/// still validates scalar values, UTF-8 and MVT semantics.
#[derive(Default)]
pub(super) struct LayerCapacity {
    pub features: usize,
    pub keys: usize,
    pub values: usize,
    pub tags: usize,
}

impl LayerCapacity {
    pub(super) fn scan(mut cur: &[u8], ctx: DecodeContext<'_>) -> Result<Self, DecodeError> {
        let mut counts = Self::default();
        while !cur.is_empty() {
            let tag = Tag::decode(&mut cur)?;
            match tag.field_number() {
                1 => {
                    check_wire_type(tag, WireType::LengthDelimited)?;
                    types::borrow_str(&mut cur)?;
                }
                5 | 15 => {
                    check_wire_type(tag, WireType::Varint)?;
                    types::decode_uint32(&mut cur)?;
                }
                2 => {
                    check_wire_type(tag, WireType::LengthDelimited)?;
                    let subctx = ctx.descend()?;
                    let feature = types::borrow_bytes(&mut cur)?;
                    counts.features += 1;
                    counts.tags += count_tags(feature, subctx)?;
                }
                3 | 4 => {
                    check_wire_type(tag, WireType::LengthDelimited)?;
                    types::borrow_bytes(&mut cur)?;
                    if tag.field_number() == 3 {
                        counts.keys += 1;
                    } else {
                        counts.values += 1;
                    }
                }
                _ => skip_field_depth(tag, &mut cur, ctx.depth())?,
            }
        }
        Ok(counts)
    }
}

fn count_tags(mut cur: &[u8], ctx: DecodeContext<'_>) -> Result<usize, DecodeError> {
    let mut count = 0;
    while !cur.is_empty() {
        let tag = Tag::decode(&mut cur)?;
        if matches!(tag.field_number(), 1 | 3) {
            check_wire_type(tag, WireType::Varint)?;
            types::decode_uint64(&mut cur)?;
        } else if matches!(tag.field_number(), 2 | 4) {
            match tag.wire_type() {
                WireType::LengthDelimited => {
                    let packed = types::borrow_bytes(&mut cur)?;
                    if tag.field_number() == 2 {
                        count += count_varints(packed);
                    }
                }
                WireType::Varint => {
                    types::decode_uint32(&mut cur)?;
                    count += usize::from(tag.field_number() == 2);
                }
                _ => {
                    return Err(buffa::encoding::wire_type_mismatch(
                        tag,
                        WireType::LengthDelimited,
                    ));
                }
            }
        } else {
            skip_field_depth(tag, &mut cur, ctx.depth())?;
        }
    }
    Ok(count)
}
