//! Wall-clock decode benchmarks (criterion), comparing fast-mvt against `mvt-reader` and `tinymvt`.
//!
//! The measured work lives in [`common::decode`] and is shared **verbatim** with the
//! instruction-count harness in `decoder_cpu.rs` — only the harness differs. Run with
//! `just bench-decode`; for deterministic CPU-instruction counts instead, see `just bench-cpu-decode`.

use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use usize_cast::FromUsize;

mod common;

use common::decode::Dec;
use common::load_repo_mvt_files;

fn bench_decode(c: &mut Criterion) {
    let tiles = load_repo_mvt_files(true);
    let bytes = tiles.iter().map(|tile| tile.bytes).sum();

    let mut group = c.benchmark_group("mvt decode");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Bytes(u64::from_usize(bytes)));
    for dec in Dec::ALL {
        group.bench_function(
            format!("{} ({} tiles)", dec.label(), tiles.len()),
            |bench| {
                bench.iter(|| common::decode::run(dec, &tiles));
            },
        );
    }
    group.finish();
}

// Include tile parsing in both cases: skipped geometry should save work before
// callers inspect IDs and properties to decide which features they need.
fn bench_selective_decode(c: &mut Criterion) {
    let tiles = load_repo_mvt_files(true);
    let mut group = c.benchmark_group("mvt selective decode");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    for (name, geometry_every) in [("properties only", 0), ("10 percent geometry", 10)] {
        group.bench_function(name, |bench| {
            bench.iter(|| {
                for tile in &tiles {
                    let reader = fast_mvt::MvtReaderRef::new(std::hint::black_box(&tile.data))
                        .expect("parse tile");
                    for layer in reader.layers() {
                        for (index, feature) in layer.features().enumerate() {
                            std::hint::black_box(feature.id());
                            for property in feature.properties() {
                                std::hint::black_box(property.expect("property"));
                            }
                            if geometry_every != 0 && index % geometry_every == 0 {
                                std::hint::black_box(feature.geometry().expect("geometry"));
                            }
                        }
                    }
                }
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_decode, bench_selective_decode);
criterion_main!(benches);
