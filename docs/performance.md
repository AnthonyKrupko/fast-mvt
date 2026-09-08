# Performance fork notes

This branch contains nine optimization commits on top of upstream
[`45a704a`](https://github.com/nyurik/fast-mvt/commit/45a704af5f63080110401fe146db40903b56fc25).
The measured implementation is
[`5afb822`](https://github.com/AnthonyKrupko/fast-mvt/commit/5afb8224c76b7304c1cb43257c32f59be66e87f5).
The documentation commit that adds this report does not change the measured code.
Upstream `main` was still at that baseline when the fork documentation was prepared.

## What changed

| Commit | Change |
|---|---|
| `99d6925` | Reserve polygon ring closure capacity. |
| `0f4531c` | Reserve complete geometry command buffers during encoding. |
| `f0c0d12` | Avoid temporary vectors when decoding singleton geometries. |
| `6f244b3` | Preallocate feature properties and encoder tag indices. |
| `bf91182` | Deduplicate borrowed values before cloning strings. |
| `d3d61e0` | Reserve owned layer feature storage. |
| `c61b75a` | Borrow encoded geometry commands and construct coordinates directly. |
| `b50d7d3` | Compact borrowed property dictionaries and resolve values once. |
| `5afb822` | Reserve layer storage and share decoded tags between features. |

The reader uses buffa's wire primitives for its structural containers and the
generated borrowed value decoder for value validation. Generated files are not
hand-edited. The writer uses an ordered value set from `indexmap`, which was
already present in the dependency graph and is now a direct optional dependency.

Property entries store a resolved value and their original wire bytes. On the
measured 64-bit target, that uses 40 bytes per entry instead of the generated
view's 96 bytes. Original bytes preserve all modeled fields through `to_proto()`,
even when several value types are populated. That conversion re-decodes values;
no general speedup is claimed for it.

Tags share one preallocated buffer per layer. A feature stores its range, and
`tags()` continues to return a slice. The capacity scan counts actual bounded
fields before allocation. A message with several faults can report a structural
error before a value error; malformed messages are still rejected.

Geometry command integers are validated during reader construction and decoded
from the original bytes during iteration. Calling `geometry()` allocates only
its geometry output, not an intermediate command vector. The result is not cached.

## Direct upstream comparison

These are Criterion measurements of the actual README benchmark bodies, not a
comparison with historical README numbers. Each workload ran original, fork,
fork, original. Both versions were freshly built with the same compiler and
locked dependencies. The table averages each version's two elapsed-time point
estimates and derives throughput from the fixed corpus size.

| Workload | Original | Fork | Original throughput | Fork throughput | Throughput gain |
|---|---:|---:|---:|---:|---:|
| Full decode | 41.359 ms | 33.436 ms | 372.72 MiB/s | 461.03 MiB/s | +23.7% |
| Borrowed encode | 5.199 ms | 4.059 ms | 164.86 MiB/s | 211.16 MiB/s | +28.1% |
| Clone + consuming encode | 7.305 ms | 6.702 ms | 117.33 MiB/s | 127.90 MiB/s | +9.0% |

[Individual estimates, confidence intervals, and accepted logs](benchmarks/2026-09-08/paired/)
are included. Averaging two estimates does not create a new combined confidence
interval. Both repeats agreed closely in this comparison.

## Allocation results

The allocation probe ran separately from the timed benchmarks. Requested bytes
include allocation and reallocation traffic, not peak or resident memory.

| Stage | Original allocations | Fork allocations | Original reallocations | Fork reallocations | Original requested bytes | Fork requested bytes |
|---|---:|---:|---:|---:|---:|---:|
| Reader construction | 375,465 | 950 | 1,093 | 36 | 191,423,488 | 51,710,576 |
| Geometry from prepared readers | 238,261 | 52,785 | 17,629 | 943 | 15,445,904 | 4,409,040 |
| Property vectors from prepared readers | 187,354 | 187,354 | 352,819 | 0 | 199,357,760 | 92,389,120 |
| Owned tiles from prepared readers | 4,158,917 | 3,973,441 | 370,939 | 943 | 335,276,325 | 163,247,661 |
| Borrowed encoding | 71,214 | 30,940 | 16,841 | 715 | 8,918,643 | 5,762,367 |

Combining reader construction and geometry, full borrowed traversal uses 613,726
versus 53,735 allocations, a 91.2% reduction. Property iteration allocates nothing.
The same stages request 206,869,392 versus 56,119,616 bytes, a 72.9% reduction.
Reader construction alone removes 99.7% of allocations. Lower allocation counts
do not imply the same percentage increase in throughput.

[Original allocation log](benchmarks/2026-09-08/allocations/original.log) and
[fork allocation log](benchmarks/2026-09-08/allocations/optimized.log) record the
fixture counts and allocation measurements. The probe
asserted equality with saved golden encoded bytes before printing these counters.

## Other libraries

The competitor suite was a separate run. Its fast-mvt estimates differ slightly
from the paired comparison above. Versions below come from `Cargo.lock`, not the
lower bounds in `Cargo.toml` or the old upstream README labels.

| Library | Full decode | Borrowed encode | Clone + consuming encode |
|---|---:|---:|---:|
| This fork | 458.92 MiB/s | 209.61 MiB/s | 127.65 MiB/s |
| tinymvt 0.3.0 | 167.57 MiB/s | 165.27 MiB/s | 108.07 MiB/s |
| mvt-reader 2.4.0 | 78.38 MiB/s | Not supported | Not supported |
| mvt 0.15.0 | Not supported | 104.04 MiB/s | 79.02 MiB/s |

[Competitor results and accepted logs](benchmarks/2026-09-08/competitors/)
include Criterion's per-case intervals. The two additional selective-decode cases
measured 27.213 ms for properties only and 28.334 ms for properties plus geometry
for every tenth feature. Those groups report elapsed time without byte throughput.

## Reproduction and limits

- Host: Apple M5, 16 GB RAM, macOS; Rust 1.95.0.
- Decode corpus: 33 sampled real-world tiles, 16,163,880 encoded bytes and 187,383
  features. Full traversal includes IDs, properties, and all geometry.
- Encode corpus: 30 sampled tiles, 898,749 original encoded bytes. The existing
  encoding selection omits the largest fixtures. Input geometry is already parsed.
- Criterion: 10 samples, 3-second warm-up and 10-second measurement target,
  extended when needed. Plot generation was disabled for these measurements.
- The direct comparison and competitor suite used the same benchmark bodies but
  separate runs. Do not mix their point estimates to infer optimization gains.
- Competing compiler jobs and active Meridian applications caused affected runs
  to be discarded and repeated. Ordinary desktop and scheduling noise remain.

From a checkout with the fixture submodule available:

```bash
git submodule update --init --recursive
just bench-decode
just bench-encode
```

To restrict the suite to fast-mvt's cases:

```bash
cargo bench --bench decoder -- 'fast-mvt traverse' --noplot
cargo bench --bench encoder -- 'fast-mvt' --noplot
```

For a paired comparison, use separate checkouts of the two recorded revisions,
compile each before timing, and alternate their benchmark executables. Clear
package artifacts between source trees if they share a target directory. During
this work, shared build state once reused an older test binary; acceptance tests
were rerun from fresh package builds.

MiB/s divides original encoded input bytes by elapsed seconds, including for
encoding. It is not an allocation rate or a count of produced output bytes. Times
are per complete corpus, not per tile. Consuming encoding includes a deep clone
and must be compared with other consuming cases rather than the borrowed cases.

These are single-threaded crate measurements, not parallel-scaling results or
Meridian application measurements. CPU-instruction-count benchmarks are separate
Valgrind/gungraun targets and were not run as part of this suite. Gains depend on
the fixture mix, hardware, and how a caller uses the reader or writer.

## Validation

Each optimization was kept in its own commit and verified independently. The
final implementation passed 350 unit/integration/runtime tests and five doc tests,
Clippy with warnings denied, reader-only and writer-only all-target checks,
formatting, and documentation generation with warnings denied.

Differential tests cover all 281 fixture files, malformed/truncated wire input,
mixed packed and unpacked fields, and decoded protobuf fidelity. Allocation tests
cover exact coordinate capacity, singleton output, borrowed string deduplication,
and shared tag storage for layers with 1, 5 and 10,000 tagged features. Those
layers each require five reader allocations and no reallocations, regardless of
feature count. Golden encoded bytes matched on the 33-tile corpus.
