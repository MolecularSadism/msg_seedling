//! Fading the whole mix: one `fade_to` on the main bus, above every category.
//!
//! For a scene about to despawn its own sound sources, which has no per-sound
//! list to fade. Nothing is tracked here: ownership is the protocol — whoever
//! engages a fade owns restoring it, so a [`FadeMix::out`] is always paired
//! with a later [`FadeMix::back`] by the same caller.
//!
//! Fades of music length run in decibel space (see [`fade_target`]): a
//! linear-amplitude ramp sags audibly down its middle and then hangs
//! inaudibly on its tail, while a dB ramp is heard as an even slide.

use std::time::Duration;

use bevy::prelude::*;
use bevy_seedling::prelude::*;

use crate::traits::AudioConfig;

/// Fades at least this long run in decibel space.
///
/// `bevy_seedling` interpolates in dB space whenever either endpoint is a
/// `Volume::Decibels`. A linear-amplitude ramp is fine for short crossfades,
/// but at music lengths it sags audibly (about −3 dB down the middle) and
/// then hangs inaudibly on its tail before vanishing.
const DB_SPACE_FADE_MIN_SECS: f32 = 0.5;

/// The dB level treated as silence by decibel-space fades, matching the
/// floor `bevy_seedling` clamps dB interpolation to.
const DB_FADE_SILENCE_DB: f32 = -60.0;

/// The `fade_to` target for a fade of `duration` toward `linear` gain:
/// linear for short fades, `Volume::Decibels` for music-length ones (see
/// [`DB_SPACE_FADE_MIN_SECS`]).
#[must_use]
pub fn fade_target(linear: f32, duration: Duration) -> Volume {
    if duration.as_secs_f32() < DB_SPACE_FADE_MIN_SECS {
        return Volume::Linear(linear);
    }
    let db = Volume::Linear(linear).decibels();
    Volume::Decibels(db.max(DB_FADE_SILENCE_DB))
}

/// Where a [`FadeMix`] points the main bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixLevel {
    /// Silence.
    Silent,
    /// Whatever the host's [`AudioConfig`] makes the mix when the fade is
    /// fired.
    Full,
}

/// Fades the whole mix to [`MixLevel`] over `duration`.
///
/// Trigger it as an event; [`MixFadePlugin`] installs the observer.
#[derive(Event, Debug, Clone, Copy)]
pub struct FadeMix {
    /// Where the bus is headed.
    pub to: MixLevel,
    /// How long the audio thread has to get there.
    pub duration: Duration,
}

impl FadeMix {
    /// Fades out to silence.
    #[must_use]
    pub fn out(duration: Duration) -> Self {
        Self {
            to: MixLevel::Silent,
            duration,
        }
    }

    /// Fades back to the level the host's config makes the mix.
    #[must_use]
    pub fn back(duration: Duration) -> Self {
        Self {
            to: MixLevel::Full,
            duration,
        }
    }
}

fn on_fade_mix<Conf: AudioConfig>(
    trigger: On<FadeMix>,
    config: Res<Conf>,
    bus: Single<(&VolumeNode, &mut AudioEvents), With<MainBus>>,
) {
    let fade = *trigger.event();
    let (volume_node, mut events) = bus.into_inner();
    let level = match fade.to {
        MixLevel::Silent => 0.0,
        MixLevel::Full => config.effective_volume(),
    };
    volume_node.fade_to(
        fade_target(level, fade.duration),
        DurationSeconds(fade.duration.as_secs_f64()),
        &mut events,
    );
}

/// Installs the [`FadeMix`] observer, reading the restore level from `Conf`.
///
/// Generic over the config type rather than the category type: the main bus
/// sits above every category, so only [`AudioConfig::effective_volume`]
/// matters here.
pub struct MixFadePlugin<Conf: AudioConfig> {
    _phantom: std::marker::PhantomData<Conf>,
}

impl<Conf: AudioConfig> Default for MixFadePlugin<Conf> {
    fn default() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<Conf: AudioConfig> Plugin for MixFadePlugin<Conf> {
    fn build(&self, app: &mut App) {
        app.init_resource::<Conf>();
        app.add_observer(on_fade_mix::<Conf>);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_fades_stay_linear() {
        let target = fade_target(0.5, Duration::from_millis(100));
        assert_eq!(target, Volume::Linear(0.5));
    }

    #[test]
    fn music_length_fades_run_in_decibel_space() {
        let target = fade_target(0.5, Duration::from_secs(2));
        let Volume::Decibels(db) = target else {
            panic!("expected a decibel-space target, got {target:?}");
        };
        assert!((db - Volume::Linear(0.5).decibels()).abs() < 1e-4);
    }

    #[test]
    fn silence_clamps_to_the_decibel_floor() {
        let target = fade_target(0.0, Duration::from_secs(2));
        assert_eq!(target, Volume::Decibels(DB_FADE_SILENCE_DB));
    }

    #[test]
    fn constructors_point_the_right_way() {
        let out = FadeMix::out(Duration::from_secs(1));
        assert_eq!(out.to, MixLevel::Silent);
        let back = FadeMix::back(Duration::from_secs(1));
        assert_eq!(back.to, MixLevel::Full);
    }
}
