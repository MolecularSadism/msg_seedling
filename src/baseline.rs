//! Per-sound baselines: the part of a sound's gain and speed that belongs to
//! the sound itself, underneath every layer that scales it.
//!
//! A sound's final volume is a product of independently-owned layers:
//!
//! ```text
//! node volume = category volume x BaseVolume x damping x duck
//! ```
//!
//! and its playback speed likewise:
//!
//! ```text
//! playback speed = BasePitch x damping
//! ```
//!
//! Only the leftmost factor comes from the host's config; everything else is
//! per-sound or per-field state. The baselines are what make that a *product*
//! rather than a race: a system that owns one layer recomputes the whole
//! expression from the components, instead of writing an absolute value and
//! hoping no one else wants a say. Without them a config change would have to
//! overwrite the node outright, throwing away the request's own
//! [`volume`](crate::PlayAudio::with_volume), its randomization, and any
//! fade it had already landed on.
//!
//! Both are inserted at spawn by [`handle_play_audio`](crate::audio_systems::handle_play_audio)
//! and by the virtual queue on promotion, so every sound this crate spawns
//! carries them. A sound spawned by hand without them reads as `1.0` on both
//! axes — the same sound it was before these existed.

use bevy::prelude::*;

/// Sanitizes an authored weight — a gain, a volume, a priority: non-finite
/// becomes `0.0`, negative clamps to `0.0`.
///
/// One rule for every number a host hands the crate to multiply with, so a
/// hostile value silences a sound rather than inverting its phase, and keeps
/// the virtual queue's ranking a total order rather than poisoning it with
/// `NaN`.
#[must_use]
pub(crate) fn sanitize_weight(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

/// The per-sound share of a sound's volume: its
/// [`PlayAudio::with_volume`](crate::PlayAudio::with_volume) multiplier times
/// whatever volume randomization drew for it, and nothing else.
///
/// Category and master volume are *not* in here — they live in the config and
/// are multiplied in by whichever system last writes the volume node. Keeping
/// the per-sound part separate is what lets a settings change re-derive the
/// node (`category x base`) without flattening the sound's own level.
///
/// A completed [`FadeOutAudio`](crate::FadeOutAudio) that keeps its entity
/// lands the sound on `0.0`: it was faded to silence deliberately, so a later
/// config change must not bring it back.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub struct BaseVolume(pub f32);

impl Default for BaseVolume {
    fn default() -> Self {
        Self(1.0)
    }
}

impl BaseVolume {
    /// Silence that survives a config change.
    pub const SILENT: Self = Self(0.0);

    /// Creates a baseline from an authored gain: non-finite becomes `0.0`,
    /// negative clamps to `0.0`. A hostile volume silences a sound rather
    /// than inverting its phase or poisoning a later multiply with `NaN`.
    #[must_use]
    pub fn new(volume: f32) -> Self {
        Self(sanitize_weight(volume))
    }

    /// The gain to write to a volume node for this sound at `category_volume`,
    /// before damping and ducking scale it further.
    #[must_use]
    pub fn resolve(self, category_volume: f32) -> f32 {
        category_volume * self.0
    }
}

/// The per-sound share of a sound's playback speed: whatever speed
/// randomization drew for it at spawn, `1.0` for an unrandomized sound.
///
/// The pitch analogue of [`BaseVolume`]. A
/// [`SoundDampingField`](crate::SoundDampingField) multiplies this baseline
/// rather than replacing it, so a muffling field bends a randomized footstep
/// down from *its* pitch instead of snapping every footstep to the same one.
///
/// Stored as `f32`, like the damping math it multiplies with: firewheel's
/// playback speed is `f32` in some releases and `f64` in others, and an `f32`
/// product widens into either losslessly.
#[derive(Component, Reflect, Clone, Copy, Debug, PartialEq)]
#[reflect(Component)]
pub struct BasePitch(pub f32);

impl Default for BasePitch {
    fn default() -> Self {
        Self(1.0)
    }
}

impl BasePitch {
    /// Creates a baseline from an authored speed, discarding a non-finite or
    /// non-positive one — neither has a sensible pitch.
    #[must_use]
    pub fn new(speed: f32) -> Self {
        if speed.is_finite() && speed > 0.0 {
            Self(speed)
        } else {
            Self(1.0)
        }
    }
}

/// Registers both baselines for reflection. Called by every plugin that
/// spawns sounds; `register_type` is idempotent.
pub(crate) fn register_types(app: &mut App) {
    app.register_type::<BaseVolume>();
    app.register_type::<BasePitch>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baselines_default_to_leaving_a_sound_alone() {
        assert_eq!(BaseVolume::default(), BaseVolume(1.0));
        assert_eq!(BasePitch::default(), BasePitch(1.0));
    }

    #[test]
    fn a_hostile_gain_silences_rather_than_poisoning_the_product() {
        assert_eq!(BaseVolume::new(f32::NAN), BaseVolume(0.0));
        assert_eq!(BaseVolume::new(f32::INFINITY), BaseVolume(0.0));
        assert_eq!(BaseVolume::new(-2.0), BaseVolume(0.0));
        assert_eq!(BaseVolume::new(0.25), BaseVolume(0.25));
    }

    #[test]
    fn a_hostile_speed_falls_back_to_unbent() {
        assert_eq!(BasePitch::new(f32::NAN), BasePitch(1.0));
        assert_eq!(BasePitch::new(0.0), BasePitch(1.0));
        assert_eq!(BasePitch::new(-1.0), BasePitch(1.0));
        assert_eq!(BasePitch::new(1.5), BasePitch(1.5));
    }

    #[test]
    fn resolve_multiplies_the_category_in() {
        assert!((BaseVolume(0.5).resolve(0.8) - 0.4).abs() < f32::EPSILON);
        assert_eq!(BaseVolume::SILENT.resolve(1.0), 0.0);
    }
}
