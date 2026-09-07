#![cfg(all(feature = "reader", feature = "writer"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

use fast_mvt::{MvtGeometry, MvtLayerBuilder, MvtReaderRef};
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
        feature
            .tag("ignored", fast_mvt::MvtValue::Null)
            .expect("null tag");
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
