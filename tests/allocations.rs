#![cfg(all(feature = "reader", feature = "writer"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

use fast_mvt::{MvtGeometry, MvtLayerBuilder, MvtReaderRef, MvtValue};
use geo_types::{LineString, MultiLineString, MultiPolygon, Polygon};

#[derive(Clone, Copy, Debug, Default)]
struct Counts {
    allocations: usize,
    reallocations: usize,
}

thread_local! {
    // Count only the test's measured region, not other tests or the test runner.
    static COUNTS: Cell<Option<Counts>> = const { Cell::new(None) };
}

fn record(reallocation: bool) {
    let _ = COUNTS.try_with(|cell| {
        if let Some(mut counts) = cell.get() {
            if reallocation {
                counts.reallocations += 1;
            } else {
                counts.allocations += 1;
            }
            cell.set(Some(counts));
        }
    });
}

struct CountingAllocator;

// All allocation operations are forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(false);
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(false);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(true);
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measure<T>(f: impl FnOnce() -> T) -> (T, Counts) {
    COUNTS.set(Some(Counts::default()));
    let value = black_box(f());
    let counts = COUNTS.take().expect("allocation measurement is active");
    (value, counts)
}

#[test]
fn encoding_geometry_never_grows_its_command_buffer() {
    let line: LineString<i32> = vec![(0, 0), (10, 0), (10, 10)].into();
    let singleton: LineString<i32> = vec![(2, 3)].into();
    let polygon = Polygon::new(line.clone(), vec![singleton.clone(), line.clone()]);
    for geometry in [
        MvtGeometry::Point((1, 2).into()),
        MvtGeometry::MultiPoint(vec![(1, 2), (3, 4)].into()),
        MvtGeometry::LineString(singleton),
        MvtGeometry::LineString(line.clone()),
        MvtGeometry::MultiLineString(MultiLineString(vec![line.clone(), line])),
        MvtGeometry::Polygon(polygon.clone()),
        MvtGeometry::MultiPolygon(MultiPolygon(vec![polygon.clone(), polygon])),
    ] {
        let layer = MvtLayerBuilder::new("test").unwrap();
        let (_, counts) = measure(|| layer.feature(black_box(&geometry)).unwrap());
        assert_eq!(counts.allocations, 1, "{geometry:?}");
        assert_eq!(counts.reallocations, 0, "{geometry:?}");
    }
}

#[test]
fn decoding_single_geometries_only_allocates_their_coordinate_buffer() {
    let line: LineString<i32> = vec![(0, 0), (10, 0), (10, 10)].into();
    for (geometry, allocations) in [
        (MvtGeometry::Point((1, 2).into()), 0),
        (MvtGeometry::LineString(line.clone()), 1),
        (MvtGeometry::Polygon(Polygon::new(line, vec![])), 1),
    ] {
        let bytes = MvtLayerBuilder::new("test")
            .unwrap()
            .feature(&geometry)
            .unwrap()
            .end()
            .encode();
        let reader = MvtReaderRef::new(&bytes).unwrap();
        let feature = reader.layers().next().unwrap().features().next().unwrap();
        let (decoded, counts) = measure(|| black_box(feature).geometry().unwrap());
        assert_eq!(decoded, geometry);
        assert_eq!(counts.allocations, allocations, "{geometry:?}");
        assert_eq!(counts.reallocations, 0, "{geometry:?}");
    }
}

#[test]
fn property_vectors_and_preallocated_tags_do_not_grow() {
    let mut feature = MvtLayerBuilder::new("test")
        .unwrap()
        .feature_with_capacity(&MvtGeometry::Point((1, 2).into()), 20)
        .unwrap();
    // Seed the dictionaries before measuring growth of this feature's tags.
    feature.tag("key", true).unwrap();
    let ((), counts) = measure(|| {
        for _ in 1..20 {
            feature.tag("key", true).expect("valid tag");
        }
        feature.tag("ignored", MvtValue::Null).expect("null tag");
    });
    assert_eq!(counts.allocations, 0);
    assert_eq!(counts.reallocations, 0);
    assert_eq!(feature.num_tags(), 20);

    let bytes = feature.end().encode();
    let reader = MvtReaderRef::new(&bytes).unwrap();
    let feature = reader.layers().next().unwrap().features().next().unwrap();
    let (properties, counts) = measure(|| black_box(feature).properties_vec().unwrap());
    assert_eq!(properties.len(), 20);
    assert_eq!(counts.allocations, 1);
    assert_eq!(counts.reallocations, 0);
    let (owned, counts) = measure(|| black_box(feature).to_feature().unwrap());
    assert_eq!(owned.properties.len(), 20);
    assert_eq!(counts.reallocations, 0);
}

#[test]
fn duplicate_string_tags_do_not_allocate() {
    let mut feature = MvtLayerBuilder::new("test")
        .unwrap()
        .feature_with_capacity(&MvtGeometry::Point((1, 2).into()), 20)
        .unwrap();
    feature.tag_string("key", "road").unwrap();
    let value = MvtValue::String("road".into());
    let ((), counts) = measure(|| {
        for _ in 0..5 {
            feature
                .tag_string("key", black_box("road"))
                .expect("string tag");
            feature
                .tag_ref("key", black_box(&value))
                .expect("borrowed tag");
        }
    });
    assert_eq!(feature.num_tags(), 11);
    assert_eq!(counts.allocations, 0);
    assert_eq!(counts.reallocations, 0);

    // A new owned string should also be moved into the table without a copy.
    let owned = String::from("a different road");
    let ((), counts) = measure(|| {
        feature
            .tag_string("key", black_box(owned))
            .expect("owned string tag");
    });
    assert_eq!(counts.allocations, 0);
    assert_eq!(counts.reallocations, 0);
}

#[test]
fn owned_layer_features_do_not_reallocate() {
    for count in [0, 1, 5, 100] {
        let mut layer = MvtLayerBuilder::with_capacity("test", count).unwrap();
        for id in 0..count {
            let mut feature = layer.feature(&MvtGeometry::Point((1, 2).into())).unwrap();
            feature.id(Some(id as u64));
            layer = feature.end();
        }
        let bytes = layer.encode();
        let reader = MvtReaderRef::new(&bytes).unwrap();
        let layer = reader.layers().next().unwrap();
        let (owned, counts) = measure(|| black_box(layer).to_layer().unwrap());
        assert_eq!(owned.features.len(), count);
        assert_eq!(counts.reallocations, 0);
        assert_eq!(counts.allocations, 1 + usize::from(count > 0));
        for (id, feature) in owned.features.iter().enumerate() {
            assert_eq!(feature.id, Some(id as u64));
            assert_eq!(feature.geometry, MvtGeometry::Point((1, 2).into()));
        }
    }
}

#[test]
fn borrowed_geometry_never_allocates_command_buffers() {
    use buffa::Message as _;
    use fast_mvt::proto::{Feature, GeomType, Layer, Tile};

    for coordinate_count in [1, 100, 10_000] {
        let mut geometry = vec![9, 0, 0];
        if coordinate_count > 1 {
            geometry.push(((coordinate_count - 1) << 3) | 2);
            for _ in 1..coordinate_count {
                geometry.extend([2, 4]);
            }
        }
        let bytes = Tile {
            layers: vec![Layer {
                version: 2,
                name: "test".into(),
                features: vec![Feature {
                    id: Some(7),
                    r#type: Some(GeomType::Linestring),
                    geometry,
                    ..Default::default()
                }],
                ..Default::default()
            }],
        }
        .encode_to_vec();
        let (reader, counts) = measure(|| MvtReaderRef::new(black_box(&bytes)).unwrap());
        // Only the layer and feature lists, independent of command count.
        assert_eq!(counts.allocations, 2);
        assert_eq!(counts.reallocations, 0);
        let feature = reader.layers().next().unwrap().features().next().unwrap();
        let ((), counts) = measure(|| {
            assert_eq!(black_box(feature).id(), Some(7));
            assert_eq!(feature.properties().count(), 0);
            for command in feature.geometry_commands() {
                black_box(command);
            }
        });
        assert_eq!(counts.allocations, 0);
        assert_eq!(counts.reallocations, 0);
        let (geometry, counts) = measure(|| feature.geometry().unwrap());
        let MvtGeometry::LineString(line) = geometry else {
            panic!("expected line");
        };
        assert_eq!(line.0.len(), coordinate_count as usize);
        assert_eq!(counts.allocations, 1);
        assert_eq!(counts.reallocations, 0);
    }
}
