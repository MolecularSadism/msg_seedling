//! Game-side evaluation of the spatial distance model.
//!
//! A spatial voice is attenuated on the audio thread by its pool's
//! `SpatialBasicNode`, and nothing reads that gain back. Anything that has to
//! rank sounds by how loud they actually arrive at the listener — the
//! [`virtual_queue`](crate::virtual_queue) above all, which must also rank
//! sounds that hold no voice and so have no node at all — evaluates the same
//! curve here, from the same parameters the pool was built with.
//!
//! [`distance_gain`] mirrors firewheel's private `DistanceModel::calculate_gain`
//! (`firewheel-core` 0.10, `dsp/distance_attenuation.rs`) model for model,
//! including the clamps `DistanceAttenuatorStereoDsp::compute_values` applies
//! to the parameters first. [`listener_distance`] mirrors how `bevy_seedling`'s
//! 2D emitter update turns world positions into the node's `offset`. Ranking and
//! hearing therefore agree for any parameters a host authors, not only for the
//! default curve.

use bevy::prelude::*;
use bevy_seedling::firewheel::dsp::distance_attenuation::{DistanceAttenuation, DistanceModel};
use bevy_seedling::prelude::SpatialBasicNode;

/// Below this, firewheel treats the rolloff factor as "no attenuation".
const MIN_GAIN_FACTOR: f32 = 0.00001;
/// Floor firewheel clamps the reference distance to.
const MIN_REFERENCE_DISTANCE: f32 = 0.00001;

/// Amplitude gain the distance model applies at `distance` audio units.
///
/// Unity inside the reference distance or with attenuation disabled; a
/// non-finite or negative distance counts as zero distance.
#[must_use]
pub fn distance_gain(params: &DistanceAttenuation, distance: f32) -> f32 {
    let distance = if distance.is_finite() {
        distance.max(0.0)
    } else {
        0.0
    };
    let reference_distance = params.reference_distance.max(MIN_REFERENCE_DISTANCE);
    let max_distance = params.max_distance.max(0.0);
    let factor = params.distance_gain_factor;

    if distance <= reference_distance || factor <= MIN_GAIN_FACTOR {
        return 1.0;
    }

    match params.distance_model {
        DistanceModel::Inverse => {
            reference_distance / (reference_distance + factor * (distance - reference_distance))
        }
        DistanceModel::Linear => {
            if max_distance <= reference_distance {
                1.0
            } else {
                (1.0 - factor * (distance - reference_distance)
                    / (max_distance - reference_distance))
                    .clamp(0.0, 1.0)
            }
        }
        DistanceModel::Exponential => (distance / reference_distance).powf(-factor),
    }
}

/// Amplitude gain a voice routed through `node` reaches the listener with at
/// `distance` audio units: the amplitude of the node's own volume times
/// [`distance_gain`],
/// clamped to silence at or below the node's `min_gain` as the audio thread
/// does.
///
/// Panning is left out: it redistributes the signal between the channels at
/// equal power, which does not change how loud the voice is.
#[must_use]
pub fn spatial_gain(node: &SpatialBasicNode, distance: f32) -> f32 {
    let gain = node.volume.amp() * distance_gain(&node.distance_attenuation, distance);
    if gain <= node.min_gain { 0.0 } else { gain }
}

/// Distance in audio units between an emitter and a 2D listener, computed the
/// way `bevy_seedling` derives a `SpatialBasicNode` offset: the world offset
/// with its depth dropped, rotated into the listener's frame, then multiplied
/// by the spatial `scale`.
#[must_use]
pub fn listener_distance(emitter: Vec3, listener: &Transform, scale: Vec3) -> f32 {
    let mut world_offset = emitter - listener.translation;
    world_offset.z = 0.0;
    let local_offset = (listener.rotation.inverse() * world_offset) * scale;
    local_offset.truncate().length()
}

/// Distance in audio units from `emitter` to the nearest of `listeners`, or
/// `None` when there is no listener to measure against.
#[must_use]
pub fn nearest_listener_distance(
    emitter: Vec3,
    listeners: &[Transform],
    scale: Vec3,
) -> Option<f32> {
    listeners
        .iter()
        .map(|listener| listener_distance(emitter, listener, scale))
        .min_by(f32::total_cmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_seedling::prelude::Volume;

    fn params(model: DistanceModel) -> DistanceAttenuation {
        DistanceAttenuation {
            distance_model: model,
            distance_gain_factor: 1.5,
            reference_distance: 2.0,
            max_distance: 12.0,
            ..Default::default()
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    /// The documented formulas of firewheel's three models, evaluated
    /// independently of the implementation under test.
    fn documented(model: DistanceModel, p: &DistanceAttenuation, d: f32) -> f32 {
        let r = p.reference_distance;
        let k = p.distance_gain_factor;
        match model {
            DistanceModel::Inverse => r / (r + k * (d.max(r) - r)),
            DistanceModel::Linear => (1.0 - k * (d - r) / (p.max_distance - r)).clamp(0.0, 1.0),
            DistanceModel::Exponential => (d.max(r) / r).powf(-k),
        }
    }

    #[test]
    fn every_model_matches_its_documented_formula_beyond_the_reference_distance() {
        for model in [
            DistanceModel::Inverse,
            DistanceModel::Linear,
            DistanceModel::Exponential,
        ] {
            let p = params(model);
            for d in [2.5_f32, 4.0, 7.3, 11.9, 30.0] {
                let got = distance_gain(&p, d);
                let want = documented(model, &p, d);
                assert!(close(got, want), "{model:?} at {d}: {got} != {want}");
            }
        }
    }

    #[test]
    fn inside_the_reference_distance_is_unity_for_every_model() {
        for model in [
            DistanceModel::Inverse,
            DistanceModel::Linear,
            DistanceModel::Exponential,
        ] {
            let p = params(model);
            for d in [0.0_f32, 1.0, p.reference_distance] {
                assert_eq!(distance_gain(&p, d), 1.0, "{model:?} at {d}");
            }
        }
    }

    #[test]
    fn exponential_with_unit_rolloff_equals_inverse() {
        let mut inverse = params(DistanceModel::Inverse);
        inverse.distance_gain_factor = 1.0;
        let exponential = DistanceAttenuation {
            distance_model: DistanceModel::Exponential,
            ..inverse
        };
        for d in [3.0_f32, 9.0, 40.0] {
            assert!(close(
                distance_gain(&inverse, d),
                distance_gain(&exponential, d)
            ));
        }
    }

    #[test]
    fn a_disabled_rolloff_never_attenuates() {
        let mut p = params(DistanceModel::Inverse);
        p.distance_gain_factor = 0.0;
        assert_eq!(distance_gain(&p, 1000.0), 1.0);
    }

    #[test]
    fn linear_without_room_past_the_reference_is_unity() {
        let mut p = params(DistanceModel::Linear);
        p.max_distance = p.reference_distance;
        assert_eq!(distance_gain(&p, 50.0), 1.0);
    }

    #[test]
    fn hostile_distances_count_as_zero() {
        let p = params(DistanceModel::Inverse);
        for d in [f32::NAN, f32::INFINITY, -4.0] {
            assert_eq!(distance_gain(&p, d), 1.0, "{d}");
        }
    }

    #[test]
    fn spatial_gain_applies_node_volume_and_min_gain() {
        let node = SpatialBasicNode {
            volume: Volume::Linear(0.5),
            distance_attenuation: params(DistanceAttenuation::default().distance_model),
            ..Default::default()
        };
        let d = 6.0;
        assert!(close(
            spatial_gain(&node, d),
            Volume::Linear(0.5).amp() * distance_gain(&node.distance_attenuation, d)
        ));

        let floored = SpatialBasicNode {
            min_gain: 0.9,
            ..node
        };
        assert_eq!(spatial_gain(&floored, d), 0.0);
    }

    #[test]
    fn listener_distance_drops_depth_and_applies_scale() {
        let listener = Transform::from_xyz(10.0, 0.0, 5.0);
        let emitter = Vec3::new(13.0, 4.0, -20.0);
        let d = listener_distance(emitter, &listener, Vec3::splat(0.5));
        assert!(close(d, 2.5));
    }

    #[test]
    fn listener_distance_rotates_into_the_listener_frame() {
        // A non-uniform scale makes the rotation observable.
        let listener =
            Transform::from_rotation(Quat::from_rotation_z(core::f32::consts::FRAC_PI_2));
        let emitter = Vec3::new(0.0, 2.0, 0.0);
        let d = listener_distance(emitter, &listener, Vec3::new(1.0, 0.0, 1.0));
        // Rotated into the listener's frame the offset lies along x, which
        // the scale keeps; along y it would have been zeroed.
        assert!(close(d, 2.0));
    }

    #[test]
    fn nearest_listener_distance_picks_the_closest() {
        let listeners = [
            Transform::from_xyz(100.0, 0.0, 0.0),
            Transform::from_xyz(3.0, 4.0, 0.0),
        ];
        let d = nearest_listener_distance(Vec3::ZERO, &listeners, Vec3::ONE);
        assert_eq!(d, Some(5.0));
        assert_eq!(nearest_listener_distance(Vec3::ZERO, &[], Vec3::ONE), None);
    }
}
