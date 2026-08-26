//! Micro-benchmarks for the per-frame damping and ducking math.
//!
//! Every input is fixed — no RNG, no wall-clock dependence — so numbers are
//! comparable across runs and commits. These cover the pure functions the
//! apply systems call per sound per frame: field influence, multi-field
//! resolution (including the geometric cutoff/speed interpolation), and the
//! ducking envelope tick.

use std::hint::black_box;

use bevy::math::Vec2;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use msg_seedling::{ActiveField, DuckingEnvelope, SoundDamping, SoundDampingField};

/// A deterministic spread of fields: centres marching along the x axis, each
/// with a distinct authored volume, cutoff, and speed so every axis of the
/// per-field accumulation does real work.
fn make_fields(count: usize) -> Vec<(Vec2, SoundDampingField)> {
    (0..count)
        .map(|i| {
            let i = i as f32;
            (
                Vec2::new(i * 4.0, 0.0),
                SoundDampingField {
                    radius: 10.0,
                    volume: 0.2 + i * 0.05,
                    cutoff_hz: 300.0 + i * 150.0,
                    speed: 0.5 + i * 0.03,
                    ..Default::default()
                },
            )
        })
        .collect()
}

/// Pairs the fields with a fixed listener, exactly as `apply_sound_damping`
/// does once per frame.
fn activate(fields: &[(Vec2, SoundDampingField)], listener: Vec2) -> Vec<ActiveField<'_>> {
    fields
        .iter()
        .map(|(centre, field)| ActiveField::new(*centre, field, Some(listener)))
        .collect()
}

fn bench_influence(c: &mut Criterion) {
    let field = SoundDampingField {
        radius: 10.0,
        volume: 0.25,
        cutoff_hz: 400.0,
        speed: 0.8,
        ..Default::default()
    };
    let mut group = c.benchmark_group("influence");
    for (label, distance) in [("centre", 0.0), ("halfway", 5.0), ("outside", 25.0)] {
        group.bench_with_input(BenchmarkId::from_parameter(label), &distance, |b, &d| {
            b.iter(|| black_box(&field).influence(black_box(d)));
        });
    }
    group.finish();
}

fn bench_resolve(c: &mut Criterion) {
    let listener = Vec2::new(2.0, 3.0);
    let source = Vec2::new(3.0, 1.0);
    let mut group = c.benchmark_group("resolve");
    for count in [1usize, 4, 16] {
        let fields = make_fields(count);
        let active = activate(&fields, listener);
        group.bench_with_input(BenchmarkId::from_parameter(count), &active, |b, active| {
            b.iter(|| SoundDamping::resolve(black_box(active), black_box(source)));
        });
    }
    group.finish();
}

fn bench_resolve_positionless(c: &mut Criterion) {
    let listener = Vec2::new(2.0, 3.0);
    let fields = make_fields(4);
    let active = activate(&fields, listener);
    c.bench_function("resolve_positionless/4", |b| {
        b.iter(|| SoundDamping::resolve_positionless(black_box(&active)));
    });
}

/// One field at half depth: both geometric axes — the log-space cutoff slide
/// and the octave-true speed bend — run their `powf` at a fractional
/// influence, the hot case for a source drifting through a field.
fn bench_geometric_axes(c: &mut Criterion) {
    let field = SoundDampingField {
        radius: 10.0,
        volume: 1.0,
        cutoff_hz: 400.0,
        speed: 0.25,
        ..Default::default()
    };
    let centre = Vec2::ZERO;
    let source = Vec2::new(5.0, 0.0);
    let active = [ActiveField::new(centre, &field, None)];
    c.bench_function("geometric_axes/half_influence", |b| {
        b.iter(|| SoundDamping::resolve(black_box(&active), black_box(source)));
    });
}

fn bench_duck_tick(c: &mut Criterion) {
    let mut attacking = DuckingEnvelope::default();
    attacking.trigger();

    // Walk a copy to mid-release: attack bottomed out, hold lapsed, gain on
    // its way back up — the envelope is `Copy`, so each iteration restarts
    // from the same frozen state.
    let mut releasing = attacking;
    releasing.tick(releasing.attack_secs);
    releasing.tick(releasing.hold_secs + 0.01);
    releasing.tick(0.05);

    let mut group = c.benchmark_group("duck_tick");
    group.bench_function("attack", |b| {
        b.iter(|| {
            let mut duck = black_box(attacking);
            duck.tick(black_box(0.008));
            duck
        });
    });
    group.bench_function("release", |b| {
        b.iter(|| {
            let mut duck = black_box(releasing);
            duck.tick(black_box(0.008));
            duck
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_influence,
    bench_resolve,
    bench_resolve_positionless,
    bench_geometric_axes,
    bench_duck_tick,
);
criterion_main!(benches);
