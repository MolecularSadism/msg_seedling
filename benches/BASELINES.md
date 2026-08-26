# Benchmark baselines

Criterion baseline `base`, captured 2026-08-26.

| | |
|---|---|
| Commit | `f96becf27d9b` |
| Branch | `claude/generic-damping-ducking` |
| Toolchain | rustc 1.98.0 (88d9e12ae 2026-08-18) |
| Host | Linux x86_64 container (shared/virtualised) |

## Results

Mean with 95% confidence interval.

| Benchmark | Mean | 95% CI |
|---|---:|---|
| `duck_tick/attack` | 7.536 ns | 7.519 ns – 7.555 ns |
| `duck_tick/release` | 7.519 ns | 7.504 ns – 7.537 ns |
| `geometric_axes/half_influence` | 22.88 ns | 22.76 ns – 23.03 ns |
| `influence/centre` | 1.877 ns | 1.865 ns – 1.891 ns |
| `influence/halfway` | 1.901 ns | 1.892 ns – 1.911 ns |
| `influence/outside` | 1.925 ns | 1.902 ns – 1.951 ns |
| `resolve/1` | 26.15 ns | 26 ns – 26.3 ns |
| `resolve/16` | 137.4 ns | 136.4 ns – 138.6 ns |
| `resolve/4` | 89.83 ns | 89.63 ns – 90.04 ns |
| `resolve_positionless/4` | 55.08 ns | 54.64 ns – 55.64 ns |

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
`--baseline base` on the same host are meaningful; comparing these absolute
numbers against a different machine — or a different toolchain — is not.

## Since

**0.4.0** left every benchmarked function byte-identical: the per-sound
baselines changed which components `apply_sound_damping` reads, not the
influence, resolution, or envelope math these cover. Verified by benchmarking
the preceding commit on the same host and comparing with `--baseline`; every
case landed inside run-to-run noise, so the table above still stands as the
reference point.

The system around them did get cheaper, in a way these micro-benchmarks do not
see: `apply_sound_damping` no longer takes `Commands`, no longer carries a
sparse-set `DampedVolumeBase` term in its query, and no longer inserts or
removes a component per sound as fields and ducks take hold and let go.
