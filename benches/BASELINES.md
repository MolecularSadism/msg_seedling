# Benchmark baselines

Criterion baseline `base`, captured 2026-08-26.

| | |
|---|---|
| Commit | `649706cb95a4` |
| Branch | `claude/branch-review-versioning-khj5m3` |
| Toolchain | rustc 1.94.1 (e408947bf 2026-03-25) |
| Host | Linux x86_64 container, 4 cores (shared/virtualised) |

## Results

Mean with 95% confidence interval.

| Benchmark | Mean | 95% CI |
|---|---:|---|
| `duck_tick/attack` | 8.711 ns | 8.641 ns – 8.792 ns |
| `duck_tick/release` | 8.027 ns | 8.009 ns – 8.046 ns |
| `geometric_axes/half_influence` | 36.04 ns | 35.71 ns – 36.45 ns |
| `influence/centre` | 3.052 ns | 2.973 ns – 3.165 ns |
| `influence/halfway` | 2.873 ns | 2.864 ns – 2.884 ns |
| `influence/outside` | 3.057 ns | 3.020 ns – 3.099 ns |
| `resolve/1` | 38.34 ns | 38.00 ns – 38.75 ns |
| `resolve/16` | 220.5 ns | 217.9 ns – 223.2 ns |
| `resolve/4` | 139.2 ns | 138.3 ns – 140.1 ns |
| `resolve_positionless/4` | 88.01 ns | 87.19 ns – 89.00 ns |

## Reproducing

```sh
cargo bench --bench hot_path -- --save-baseline base   # capture
cargo bench --bench hot_path -- --baseline base        # compare against it
```

`--bench hot_path` is required, not tidiness: a bare `cargo bench` hands
criterion's flags to the lib test harness first, which rejects them with
`error: Unrecognized option: 'baseline'`.

These were taken in a shared virtualised container, so absolute figures carry
more run-to-run noise than a dedicated machine. Comparisons made with
`--baseline base` on the same host and toolchain are meaningful; comparing
these absolute numbers against either a different machine or a different
toolchain is not — see below for what that looks like in practice.

## Previous captures

| Benchmark | 0.4.0 (above) | 0.3.1 |
|---|---:|---:|
| `duck_tick/attack` | 8.711 ns | 7.536 ns |
| `duck_tick/release` | 8.027 ns | 7.519 ns |
| `geometric_axes/half_influence` | 36.04 ns | 22.88 ns |
| `influence/centre` | 3.052 ns | 1.877 ns |
| `influence/halfway` | 2.873 ns | 1.901 ns |
| `influence/outside` | 3.057 ns | 1.925 ns |
| `resolve/1` | 38.34 ns | 26.15 ns |
| `resolve/16` | 220.5 ns | 137.4 ns |
| `resolve/4` | 139.2 ns | 89.83 ns |
| `resolve_positionless/4` | 88.01 ns | 55.08 ns |

The 0.3.1 column was captured at `f96becf27d9b` on rustc 1.98.0 (88d9e12ae
2026-08-18), in a different container.

**That column is not a regression, and the two columns do not compare.** Every
benchmarked function is byte-identical between the two: 0.4.0's per-sound
baselines changed which components `apply_sound_damping` reads, not the
influence, resolution, or envelope math these cover. The gap is the host and
the older, faster toolchain, which is exactly the caveat above — measured, not
assumed: the commit preceding the 0.4.0 work was benchmarked on *this* host and
compared with `--baseline`. Criterion called three of the ten significant, and
it called them in both directions — `resolve/1` +2.1%, `resolve/4` −5.0%,
`resolve/16` −5.8% — which for byte-identical machine code is code layout and
container noise rather than any effect of the change. Nothing moved by
anything close to the 1.5x the column above shows.

The system around those functions did get cheaper, in a way these
micro-benchmarks do not see: `apply_sound_damping` no longer takes `Commands`,
no longer carries a sparse-set `DampedVolumeBase` term in its query, and no
longer inserts or removes a component per sound as fields and ducks take hold
and let go.
