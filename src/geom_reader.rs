use geo_types::{Coord, LineString, MultiLineString, MultiPoint, MultiPolygon, Point, Polygon};

use crate::generated::vector_tile::tile::GeomType;
use crate::geom::{Command, signed_area};
use crate::{MvtCoord, MvtError, MvtGeometry, MvtLineString, MvtResult};

pub(crate) fn decode_geometry(
    geom_type: Option<GeomType>,
    commands: impl ExactSizeIterator<Item = u32> + Clone,
) -> MvtResult<MvtGeometry> {
    if commands.len() == 0 {
        return match geom_type {
            Some(GeomType::Point) => Ok(MultiPoint(Vec::new()).into()),
            Some(GeomType::Linestring) => Ok(MultiLineString(Vec::new()).into()),
            Some(GeomType::Polygon) => Ok(MultiPolygon(Vec::new()).into()),
            _ => Err(MvtError::InvalidGeometry),
        };
    }
    match geom_type {
        Some(GeomType::Point) => parse_points(commands),
        Some(GeomType::Linestring) => parse_linestrings(commands),
        Some(GeomType::Polygon) => parse_polygons(commands),
        _ => Err(MvtError::InvalidGeometry),
    }
}

fn decode_coord(
    cursor: &mut MvtCoord,
    values: &mut impl Iterator<Item = u32>,
) -> MvtResult<MvtCoord> {
    let dx = values.next().ok_or(MvtError::InvalidGeometry)?;
    let dy = values.next().ok_or(MvtError::InvalidGeometry)?;
    cursor.x = saturating_add_delta(cursor.x, dx);
    cursor.y = saturating_add_delta(cursor.y, dy);
    Ok(*cursor)
}

pub(crate) fn saturating_add_delta(value: i32, delta: u32) -> i32 {
    let v = (delta >> 1).cast_signed() ^ -(delta & 1).cast_signed();
    value.saturating_add(v)
}

fn parse_points(mut data: impl ExactSizeIterator<Item = u32>) -> MvtResult<MvtGeometry> {
    let mut cursor = Coord { x: 0, y: 0 };
    let mut first = None;
    let mut points = Vec::new();
    while let Some(header) = data.next() {
        let (command, mut count) = Command::decode(header)?;
        if command != Command::MoveTo || count > data.len() / 2 {
            return Err(MvtError::InvalidGeometry);
        }
        if count > 0 && first.is_none() {
            first = Some(Point(decode_coord(&mut cursor, &mut data)?));
            count -= 1;
        }
        if count > 0 {
            points.reserve(count + usize::from(points.is_empty()));
            if points.is_empty() {
                points.push(first.ok_or(MvtError::InvalidGeometry)?);
            }
        }
        for _ in 0..count {
            points.push(Point(decode_coord(&mut cursor, &mut data)?));
        }
    }
    if points.is_empty() {
        Ok(first.ok_or(MvtError::InvalidGeometry)?.into())
    } else {
        Ok(MultiPoint(points).into())
    }
}

#[derive(Debug, Copy, Clone)]
struct LineInfo {
    move_count: usize,
    line_count: Option<usize>,
}

// Look ahead only past MoveTo coordinates (normally one pair) to reserve the
// entire line, including a polygon's closure, before decoding its coordinates.
fn line_info(mut data: impl ExactSizeIterator<Item = u32>) -> MvtResult<LineInfo> {
    let (move_to, move_count) = Command::decode(data.next().ok_or(MvtError::InvalidGeometry)?)?;
    if move_to != Command::MoveTo || move_count == 0 || move_count > data.len() / 2 {
        return Err(MvtError::InvalidGeometry);
    }
    data.nth(move_count * 2 - 1);
    let mut line_count = None;
    if let Some(header) = data.next() {
        let (line_to, count) = Command::decode(header)?;
        if line_to == Command::LineTo {
            if count > data.len() / 2 {
                return Err(MvtError::InvalidGeometry);
            }
            line_count = Some(count);
        }
    }
    Ok(LineInfo {
        move_count,
        line_count,
    })
}

fn parse_line(
    cursor: &mut MvtCoord,
    data: &mut impl Iterator<Item = u32>,
    info: LineInfo,
    reserve_closure: bool,
) -> MvtResult<MvtLineString> {
    data.next(); // MoveTo, validated by line_info.
    let mut coords = Vec::with_capacity(
        info.move_count + info.line_count.unwrap_or(0) + usize::from(reserve_closure),
    );
    for _ in 0..info.move_count {
        coords.push(decode_coord(cursor, data)?);
    }
    if let Some(count) = info.line_count {
        data.next(); // LineTo, validated by line_info.
        for _ in 0..count {
            coords.push(decode_coord(cursor, data)?);
        }
    }
    Ok(LineString(coords))
}

fn parse_linestrings(
    mut data: impl ExactSizeIterator<Item = u32> + Clone,
) -> MvtResult<MvtGeometry> {
    let mut cursor = Coord { x: 0, y: 0 };
    let info = line_info(data.clone())?;
    let first = parse_line(&mut cursor, &mut data, info, false)?;
    if data.len() == 0 {
        return Ok(first.into());
    }
    let mut lines = Vec::new();
    lines.push(first);
    while data.len() > 0 {
        let info = line_info(data.clone())?;
        lines.push(parse_line(&mut cursor, &mut data, info, false)?);
    }
    Ok(MultiLineString(lines).into())
}

fn parse_ring(
    cursor: &mut MvtCoord,
    data: &mut (impl ExactSizeIterator<Item = u32> + Clone),
) -> MvtResult<(MvtLineString, i64)> {
    let info = line_info(data.clone())?;
    let mut ring = parse_line(cursor, data, info, true)?;
    let (close, close_count) = Command::decode(data.next().ok_or(MvtError::InvalidGeometry)?)?;
    if close != Command::ClosePath || close_count != 1 {
        return Err(MvtError::InvalidGeometry);
    }
    let area = signed_area(&ring.0);
    let first = *ring.0.first().ok_or(MvtError::InvalidGeometry)?;
    ring.0.push(first);
    Ok((ring, area))
}

fn parse_polygons(mut data: impl ExactSizeIterator<Item = u32> + Clone) -> MvtResult<MvtGeometry> {
    let mut cursor = MvtCoord::zero();
    let mut current_exterior: Option<MvtLineString> = None;
    let mut current_interiors = Vec::new();
    let mut polygons = Vec::new();
    while data.len() > 0 {
        let (ring, area) = parse_ring(&mut cursor, &mut data)?;
        if area > 0 || current_exterior.is_none() {
            if let Some(exterior) = current_exterior.replace(ring) {
                polygons.push(Polygon::new(
                    exterior,
                    std::mem::take(&mut current_interiors),
                ));
            }
        } else {
            current_interiors.push(ring);
        }
    }
    let exterior = current_exterior.ok_or(MvtError::InvalidGeometry)?;
    let last = Polygon::new(exterior, current_interiors);
    if polygons.is_empty() {
        Ok(last.into())
    } else {
        polygons.push(last);
        Ok(MultiPolygon(polygons).into())
    }
}

#[cfg(all(test, feature = "writer", feature = "reader"))]
mod tests {
    use geo_types::{Geometry, coord};

    use super::*;

    #[test]
    fn singleton_points_validate_the_entire_command_stream() {
        for commands in [vec![9, 4, 6], vec![1, 9, 4, 6, 1]] {
            assert_eq!(
                parse_points(commands.iter().copied()).unwrap(),
                Point::new(2, 3).into()
            );
        }
        for commands in [vec![9, 4, 6, 10], vec![9, 4, 6, 9], vec![1, 1]] {
            assert!(parse_points(commands.iter().copied()).is_err());
        }
        let commands = [1, 9, 4, 6, 1, 17, 2, 4, 2, 4, 1];
        assert_eq!(
            parse_points(commands.iter().copied()).unwrap(),
            MultiPoint::from(vec![(2, 3), (3, 5), (4, 7)]).into()
        );
    }

    #[test]
    fn parses_move_only_lines_and_polygons() {
        let Geometry::LineString(line) = parse_linestrings([9, 4, 6].into_iter()).unwrap() else {
            panic!("expected line");
        };
        assert_eq!(line.0, vec![coord! { x: 2, y: 3 }]);

        let Geometry::MultiLineString(lines) =
            parse_linestrings([9, 4, 6, 9, 2, 2].into_iter()).unwrap()
        else {
            panic!("expected multiline");
        };
        assert_eq!(lines.0.len(), 2);

        let Geometry::Polygon(poly) = parse_polygons([9, 0, 0, 15].into_iter()).unwrap() else {
            panic!("expected polygon");
        };
        assert_eq!(
            poly.exterior().0,
            vec![coord! { x: 0, y: 0 }, coord! { x: 0, y: 0 }]
        );
    }

    #[test]
    fn direct_helper_edge_paths_are_covered() {
        assert!(matches!(
            line_info([10, 0, 0].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
        assert!(matches!(
            line_info([17, 0, 0].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
        assert!(matches!(
            parse_ring(&mut MvtCoord::zero(), &mut [9, 0, 0].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
        assert!(matches!(
            parse_ring(&mut MvtCoord::zero(), &mut [9, 0, 0, 8].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));

        let Geometry::Polygon(poly) = parse_polygons([17, 0, 0, 10, 0, 15].into_iter()).unwrap()
        else {
            panic!("expected polygon");
        };
        assert_eq!(poly.exterior().0.len(), 3);
    }

    #[test]
    fn invalid_geometry_streams_are_rejected() {
        assert!(matches!(
            decode_coord(&mut coord! { x: 0, y: 0 }, &mut [1].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
        assert!(matches!(
            parse_points([Command::MoveTo.encode(0x1fff_ffff).unwrap(), 0, 0].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
        assert!(matches!(
            parse_points([].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
        assert!(matches!(
            parse_points([17, 1, 2].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
        assert!(matches!(
            parse_points([10, 0, 0].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
        assert!(matches!(
            parse_linestrings([].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
        assert!(matches!(
            parse_linestrings([9, 0].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
        assert!(matches!(
            parse_linestrings([9, 0, 0, 18].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
        assert!(matches!(
            parse_polygons([].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
        assert!(matches!(
            parse_polygons([9, 0, 0, 8].into_iter()),
            Err(MvtError::InvalidGeometry)
        ));
    }
}

#[cfg(test)]
mod allocation_tests {
    use super::*;

    #[test]
    fn ring_capacity_includes_closure_without_spare_coordinates() {
        for commands in [
            vec![9, 0, 0, 26, 20, 0, 0, 20, 19, 0, 15],
            vec![9, 4, 6, 15],
            vec![9, 0, 0, 34, 20, 0, 0, 20, 19, 0, 0, 19, 15],
        ] {
            let mut data = commands.iter().copied();
            let (ring, _) = parse_ring(&mut MvtCoord::zero(), &mut data).unwrap();
            assert_eq!(data.len(), 0);
            assert_eq!(ring.0.capacity(), ring.0.len());
            assert_eq!(ring.0.first(), ring.0.last());
        }
    }

    #[test]
    fn line_capacity_does_not_include_a_closure_slot() {
        let commands = [9, 0, 0, 18, 20, 0, 0, 20];
        let info = line_info(commands.iter().copied()).unwrap();
        let line = parse_line(
            &mut MvtCoord::zero(),
            &mut commands.into_iter(),
            info,
            false,
        )
        .unwrap();
        assert_eq!(line.0.len(), 3);
        assert_eq!(line.0.capacity(), 3);
    }
}
