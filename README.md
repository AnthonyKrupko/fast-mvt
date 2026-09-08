# fast-mvt: performance fork

This fork of [nyurik/fast-mvt](https://github.com/nyurik/fast-mvt) reduces allocation
and copying in the Rust Mapbox Vector Tile reader and writer.

The fork keeps the upstream integer geometry model and MVT wire format. Geometry
uses [`geo-types`](https://docs.rs/geo-types/latest/geo_types/) with `i32` tile-space
coordinates. Callers provide projected integer coordinates; this crate does not
project, scale, or round geographic coordinates.

Against upstream commit [`45a704a`](https://github.com/nyurik/fast-mvt/commit/45a704af5f63080110401fe146db40903b56fc25),
the optimized code at [`5afb822`](https://github.com/AnthonyKrupko/fast-mvt/commit/5afb8224c76b7304c1cb43257c32f59be66e87f5)
measured 24% higher decode throughput and 28% higher borrowed-encode throughput on
an Apple M5. Reader construction used 99.7% fewer allocations.
The [benchmarks](#benchmarks) below include the workloads and raw results.

## Differences from upstream

| Area | Changes in this fork |
|---|---|
| Geometry commands | Borrow command bytes from the input. Decode coordinates when requested. |
| Property dictionaries | Decode each value once into compact storage. Keep the original bytes for `to_proto()`. |
| Feature tags | Store tags in one buffer per layer. Each feature's `tags()` returns a slice of that buffer. |
| Reader storage | Count fields before decoding a layer to reserve space for features, dictionaries, and tags. |
| Geometry and owned output | Reserve output space, including polygon ring closure, and remove temporary vectors for singleton geometries. |
| Encoding | Reserve command and tag buffers. Check for duplicate values before copying strings. |

See the [performance notes](docs/performance.md) for individual commits and tests.

### Rust API and behavior differences

- `geometry_commands()` returns a cloneable `MvtGeometryCommands` iterator with
  `Item = u32`, replacing the upstream `&[u32]`. Use `.len()` for its count and
  `.collect::<Vec<u32>>()` when an owned command buffer is required.
- `tag_ref()` accepts borrowed values; `feature_with_capacity()` reserves tag pairs.
  `tag_string()` accepts `Into<Cow<str>>`. Custom types that only implement
  `Into<String>` need an explicit conversion before calling it.
- Reader construction still validates protobuf wire data. Its structural sizing
  pass can report a different first error for a message containing several faults.
- `to_proto()` decodes compacted value messages again from their original bytes.
  This extra work can make owned protobuf conversion slower.

## Installation

Use the fork's Git source to get these changes. The package name remains `fast-mvt`;
the crates.io package and upstream release binaries do not include these changes.

```toml
[dependencies]
fast-mvt = { git = "https://github.com/AnthonyKrupko/fast-mvt" }
```

For reproducible builds, pin an exact commit with `rev`. For a
reader-only dependency, set `default-features = false` and `features = ["reader"]`.

Install the fork's CLI from source:

```bash
cargo install fast-mvt --git https://github.com/AnthonyKrupko/fast-mvt \
  --locked --features cli --bin mvt
```

## Decoding a tile

```rust
use fast_mvt::{MvtReaderRef, MvtResult};

fn read_tile(bytes: &[u8]) -> MvtResult<()> {
    let reader = MvtReaderRef::new(bytes)?;

    for layer in reader.layers() {
        for feature in layer.features() {
            let geometry = feature.geometry()?;
            println!("geo: {geometry:?}");
            let id = feature.id();
            println!("id: {id:?}");

            for property in feature.properties() {
                let (key, value) = property?;
                println!("{key} = {value:?}");
            }
        }
    }

    Ok(())
}
```

Reading IDs and properties does not allocate geometry command buffers.
`feature.geometry()` decodes coordinates each time you call it.
Keep the returned geometry if you need it again. Use `feature.geometry_commands()`
to inspect the commands without building coordinates.

## Encoding a tile

Start with `MvtLayerBuilder::new()`, add a feature and its tags, then encode the
layer. `feature.end()` adds the feature to the layer and returns the layer builder.

```rust
use fast_mvt::{MvtGeometry, MvtLayerBuilder, MvtResult};

fn create_tile() -> MvtResult<Vec<u8>> {
    let layer = MvtLayerBuilder::new("places")?;

    let mut feature = layer.feature(&MvtGeometry::Point((1, 2).into()))?;
    feature.id(Some(7));
    feature.tag_string("name", "Example")?;
    feature.tag("visible", true)?;

    Ok(feature.end().encode())
}
```

Use `tag_string()` for borrowed or owned strings and `tag_ref()` for borrowed
`MvtValue` values. They look up existing layer values before copying string data.
`tag()` accepts owned values. `MvtTile::encode_ref()` also checks for duplicates
before copying strings.

To add another layer, call `.layer()` on the current layer builder:

```rust
use fast_mvt::{MvtGeometry, MvtLayerBuilder, MvtResult};

fn create_tile() -> MvtResult<Vec<u8>> {
    let mut feature = MvtLayerBuilder::new("roads")?
        .feature(&MvtGeometry::Point((1, 2).into()))?;
    feature.tag("kind", "residential")?;

    let mut feature = feature.end().layer("water")?
        .feature(&MvtGeometry::Point((3, 4).into()))?;
    feature.tag("name", "Lake")?;

    Ok(feature.end().encode())
}
```

Use `MvtTileBuilder` to reserve space for a known number of layers and features.
With this builder, `feature.end()` returns the layer and `layer.end()` returns
the tile:

```rust
use fast_mvt::{MvtGeometry, MvtResult, MvtTileBuilder};

fn write_tile() -> MvtResult<Vec<u8>> {
    let tile = MvtTileBuilder::with_capacity(1);
    let layer = tile.layer_with_capacity("places", 1)?;

    let mut feature = layer.feature(&MvtGeometry::Point((1, 2).into()))?;
    feature.id(Some(7));
    feature.tag("name", "Example")?;
    feature.tag("visible", true)?;

    Ok(feature.end().end().encode())
}
```

## Parallel encoding

Each layer has its own key and value dictionaries, so separate threads can encode
layers independently. Concatenate their `MvtLayerBuilder::encode()` outputs in the
desired layer order to form a tile.

This example uses a serial iterator. With Rayon and its parallel iterator traits
in scope, replace `.iter()` with `.par_iter()` to encode layers in parallel.

```rust
use fast_mvt::{MvtGeometry, MvtLayerBuilder, MvtResult};

fn encode_layer(name: &str, geometry: &MvtGeometry) -> MvtResult<Vec<u8>> {
    let feature = MvtLayerBuilder::new(name)?.feature(geometry)?;
    Ok(feature.end().encode())
}

fn encode_tile(layers: &[(&str, MvtGeometry)]) -> MvtResult<Vec<u8>> {
    let buffers: Vec<Vec<u8>> = layers
        .iter()
        .map(|(name, geometry)| encode_layer(name, geometry))
        .collect::<MvtResult<_>>()?;

    Ok(buffers.concat())
}
```

## Benchmarks

### Original upstream versus this fork

Criterion compared upstream `45a704a` with fork `5afb822`, which includes all nine
optimization commits. Both ran on the same Apple M5 with the same compiler,
dependencies, fixtures, and benchmark settings.

| Workload | Original time | Fork time | Original throughput | Fork throughput | Throughput gain |
|---|---:|---:|---:|---:|---:|
| Full decode | 41.359 ms | 33.436 ms | 372.72 MiB/s | 461.03 MiB/s | +23.7% |
| Borrowed encode | 5.199 ms | 4.059 ms | 164.86 MiB/s | 211.16 MiB/s | +28.1% |
| Clone + consuming encode | 7.305 ms | 6.702 ms | 117.33 MiB/s | 127.90 MiB/s | +9.0% |

Decode covers 33 real-world tiles, totaling 16,163,880 encoded bytes. Encode covers
30 tiles, totaling 898,749 original encoded bytes. Times are per complete corpus,
not per tile. The consuming benchmark includes a deep clone of its prepared input.

Reader construction uses 950 allocations instead of 375,465. Combining reader
construction and geometry output, full traversal uses 91.2% fewer allocations and
requests 72.9% fewer bytes from the allocator. These counts measure allocation
traffic, not peak memory or resident memory.

### Other libraries

A separate Criterion run on the same machine compared the locked dependency
versions. Compare throughput within each column; decoding and encoding use
different corpora, and the consuming path includes input cloning.

| Library | Full decode | Borrowed encode | Clone + consuming encode |
|---|---:|---:|---:|
| This fork, `5afb822` | 458.92 MiB/s | 209.61 MiB/s | 127.65 MiB/s |
| `tinymvt 0.3.0` | 167.57 MiB/s | 165.27 MiB/s | 108.07 MiB/s |
| `mvt-reader 2.4.0` | 78.38 MiB/s | Not supported | Not supported |
| `mvt 0.15.0` | Not supported | 104.04 MiB/s | 79.02 MiB/s |

### Running the benchmarks

```bash
just bench-decode
just bench-encode
```

The benches use 10 samples, a 3-second warm-up, and a 10-second measurement target.
The direct comparison ran original/fork/fork/original for each workload. Runs with
competing compiler or Meridian processes were discarded and repeated.

Fewer allocations leave more time for parsing and geometry work, but a 99.7% drop
in allocations does not mean a 99.7% drop in runtime. These benchmarks measure
single-threaded crate throughput. They do not measure parallel scaling or
Meridian's tile pipeline, and results will vary with the tile set and machine.

See [methods, allocation results, and raw benchmark data](docs/performance.md) for
the full comparison.

## Features

| Feature     | Purpose                                                                 |
|-------------|-------------------------------------------------------------------------|
| `reader`    | MVT tile decoding from bytes.                                           |
| `writer`    | MVT tile encoding into bytes.                                           |
| `json`      | Enable serde JSON support.                                              |
| `codegen`   | Regenerate checked-in protobuf bindings from `src/vector_tile.proto`.   |
| `arbitrary` | Derive `arbitrary::Arbitrary` for generated protobuf types for fuzzing. |

The generated protobuf files are checked in, so normal builds do not require
`protoc`. Run `just update-generated` to refresh generated code after `buffa` upgrades.

## Development

Install [just](https://github.com/casey/just#readme) with `cargo install just`.
Run `just` to list commands or `just test` to run the tests.

## Credits

This fork builds on Yuri Astrakhan's [fast-mvt](https://github.com/nyurik/fast-mvt).
Upstream credits these projects for encoding, decoding, and tests:

- Encoding and testing from `mvt` crate by the Minnesota Department of Transportation.
- Decoding and testing from `mvt-reader` by Paul Lange.

## License

Licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
* MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)
  at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the
Apache-2.0 license, shall be dual-licensed as above, without any
additional terms or conditions.
