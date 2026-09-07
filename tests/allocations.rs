#![cfg(all(feature = "reader", feature = "writer"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

use fast_mvt::{MvtGeometry, MvtLayerBuilder};
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
