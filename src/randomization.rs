use bevy::prelude::*;

/// Controls how volume and speed are randomized per sound.
///
/// When a [`PlayAudio`](crate::PlayAudio) message is sent, the randomization
/// setting determines how much variation is applied.
///
/// - `Default` — uses the plugin-configured [`DefaultRandomization`].
/// - `Volume(f32)` — custom volume deviation, speed uses plugin default.
/// - `Speed(f32)` — custom speed deviation, volume uses plugin default.
/// - `VolumeAndSpeed { .. }` — custom deviations for both.
///
/// Deviation values represent a ± range around 1.0. For example, `0.2` means
/// the final value will be randomly chosen in `[0.8, 1.2]`.
#[derive(Clone, Copy, Debug, Default)]
pub enum Randomization {
    /// Use plugin-configured defaults for both volume and speed.
    #[default]
    Default,
    /// Custom volume deviation; speed uses plugin default.
    Volume(f32),
    /// Custom speed deviation; volume uses plugin default.
    Speed(f32),
    /// Custom deviations for both volume and speed.
    VolumeAndSpeed { volume: f32, speed: f32 },
}

/// Resource that stores the plugin-wide default randomization settings.
///
/// Configured via [`MsgSeedlingPlugin`](crate::MsgSeedlingPlugin) at init.
/// Defaults to `Some(0.2)` (±20%) for both volume and speed.
///
/// `None` means no randomization for that axis.
#[derive(Resource, Clone, Debug)]
pub struct DefaultRandomization {
    /// Default volume deviation. `Some(0.2)` = ±20%.
    pub volume: Option<f32>,
    /// Default speed deviation. `Some(0.2)` = ±20%.
    pub speed: Option<f32>,
}

impl Default for DefaultRandomization {
    fn default() -> Self {
        Self {
            volume: Some(0.2),
            speed: Some(0.2),
        }
    }
}

/// Resolves a [`Randomization`] value against plugin defaults.
///
/// Returns `(volume_deviation, speed_deviation)` as `Option<f32>` each.
/// `None` means no randomization for that axis.
pub fn resolve_randomization(
    randomization: Randomization,
    defaults: &DefaultRandomization,
) -> (Option<f32>, Option<f32>) {
    match randomization {
        Randomization::Default => (defaults.volume, defaults.speed),
        Randomization::Volume(v) => (Some(v), defaults.speed),
        Randomization::Speed(s) => (defaults.volume, Some(s)),
        Randomization::VolumeAndSpeed { volume, speed } => (Some(volume), Some(speed)),
    }
}

/// Draws `centre` scaled by a random factor in `1 ± deviation`.
///
/// `None` (or a non-positive or non-finite deviation) returns `centre`
/// untouched, so an unrandomized axis costs nothing and never consumes an RNG
/// draw. A deviation wider than `1.0` floors the low end at zero rather than
/// flipping the sign, so the widest possible spread is `0..=2 × centre`.
///
/// Used for both axes: volume deviates around the request's own volume,
/// speed around `1.0`.
#[cfg(feature = "rand")]
pub fn deviate(rng: &mut impl rand::Rng, centre: f32, deviation: Option<f32>) -> f32 {
    let Some(deviation) = deviation.filter(|d| d.is_finite() && *d > 0.0) else {
        return centre;
    };
    let min = centre * (1.0 - deviation).max(0.0);
    let max = centre * (1.0 + deviation);
    if min < max {
        rng.random_range(min..=max)
    } else {
        // A zero (or degenerate) centre has nothing to deviate around.
        min.min(max)
    }
}

/// System-local RNG for the crate's spawn-time randomization.
///
/// Both axes are drawn here rather than deferring the speed to seedling's
/// [`RandomPitch`](bevy_seedling::prelude::RandomPitch), so a sound's
/// [`BasePitch`](crate::BasePitch) is known at spawn instead of a frame later.
#[cfg(feature = "rand")]
pub struct AudioRng(pub rand::rngs::SmallRng);

#[cfg(feature = "rand")]
impl FromWorld for AudioRng {
    fn from_world(_world: &mut World) -> Self {
        use rand::SeedableRng;
        Self(rand::rngs::SmallRng::from_rng(&mut rand::rng()))
    }
}
