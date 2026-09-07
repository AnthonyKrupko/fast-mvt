use std::iter::FusedIterator;

use buffa::encoding::{Tag, WireType, skip_field_depth};
use buffa::{DecodeError, types};

/// Borrowed geometry fields, validated once while reading the feature. The tail
/// starts at the first geometry tag and includes any later, interleaved fields.
/// Keeping one slice avoids allocating a fragment list for split packed fields.
#[derive(Clone, Debug, Default)]
pub(super) struct GeometryFields<'a> {
    source: &'a [u8],
    count: usize,
}

impl<'a> GeometryFields<'a> {
    pub(super) fn read(
        &mut self,
        tag: Tag,
        before_tag: &'a [u8],
        cur: &mut &'a [u8],
    ) -> Result<(), DecodeError> {
        if self.source.is_empty() {
            self.source = before_tag;
        }
        match tag.wire_type() {
            WireType::LengthDelimited => {
                let mut packed = types::borrow_bytes(cur)?;
                while !packed.is_empty() {
                    types::decode_uint32_packed(&mut packed)?;
                    self.count += 1;
                }
            }
            WireType::Varint => {
                types::decode_uint32(cur)?;
                self.count += 1;
            }
            _ => {
                return Err(buffa::encoding::wire_type_mismatch(
                    tag,
                    WireType::LengthDelimited,
                ));
            }
        }
        Ok(())
    }

    pub(super) fn iter(&self) -> MvtGeometryCommands<'a> {
        let mut iter = MvtGeometryCommands {
            fields: self.source,
            packed: &[],
            remaining: self.count,
        };
        // Most features have one packed field. Enter its payload once here so
        // cloned lookahead cursors do not repeat the tag and length decoding.
        if self.count > 0 {
            let mut fields = self.source;
            let tag = Tag::decode(&mut fields).expect("validated geometry tag");
            if tag.wire_type() == WireType::LengthDelimited {
                iter.packed = types::borrow_bytes(&mut fields).expect("validated packed geometry");
                iter.fields = fields;
            }
        }
        iter
    }
}

/// An allocation-free iterator over a feature's geometry command integers.
///
/// Integers are read from the original tile bytes as needed. Wire validity is
/// checked by [`crate::MvtReaderRef::new`]; MVT geometry validity is checked by
/// [`crate::MvtFeatureRef::geometry`]. Clone the iterator to traverse it again,
/// or collect it into a `Vec<u32>` when an owned command buffer is needed.
#[derive(Clone, Debug)]
pub struct MvtGeometryCommands<'a> {
    fields: &'a [u8],
    packed: &'a [u8],
    remaining: usize,
}

impl MvtGeometryCommands<'_> {
    fn next_field(&mut self) -> Option<u32> {
        loop {
            let tag = Tag::decode(&mut self.fields).expect("validated feature tag");
            if tag.field_number() == 4 {
                if tag.wire_type() == WireType::LengthDelimited {
                    self.packed =
                        types::borrow_bytes(&mut self.fields).expect("validated packed geometry");
                    if !self.packed.is_empty() {
                        return self.next();
                    }
                } else {
                    self.remaining -= 1;
                    return Some(
                        types::decode_uint32(&mut self.fields).expect("validated geometry varint"),
                    );
                }
            } else {
                skip_field_depth(tag, &mut self.fields, buffa::RECURSION_LIMIT)
                    .expect("validated feature field");
            }
        }
    }
}

impl Iterator for MvtGeometryCommands<'_> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        if self.packed.is_empty() {
            return self.next_field();
        }
        self.remaining -= 1;
        Some(types::decode_uint32_packed(&mut self.packed).expect("validated geometry varint"))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for MvtGeometryCommands<'_> {}
impl FusedIterator for MvtGeometryCommands<'_> {}
