//! Fading the whole mix: one `fade_to` on the main bus, above every category.
//!
//! For a scene about to despawn its own sound sources, which has no per-sound
//! list to fade. Ownership is the protocol — whoever engages a fade owns
//! restoring it, so a [`FadeMix::out`] is always paired with a later
//! [`FadeMix::back`] by the same caller. The one piece of bookkeeping is
//! [`MixFadeState`]: the observer records where the bus was last pointed so
//! the config-driven master-volume write stands down while the mix is away
//! at [`MixLevel::Silent`].
//!
//! Fades of music length run in decibel space (see [`fade_target`]): a
//! linear-amplitude ramp sags audibly down its middle and then hangs
//! inaudibly on its tail, while a dB ramp is heard as an even slide.
//!
//! ## Example
//!
//! ```
//! # use std::time::Duration;
//! # use bevy::prelude::*;
//! # use msg_seedling::prelude::*;
//! # #[derive(Resource, Clone, Default)]
//! # struct GameAudioConfig;
//! # impl AudioConfig for GameAudioConfig {
//! #     fn master_volume(&self) -> f32 { 1.0 }
//! # }
//! # let mut app = App::new();
//! # app.add_plugins(MinimalPlugins);
//! // Generic over the config, not the category: the bus sits above both.
//! app.add_plugins(MixFadePlugin::<GameAudioConfig>::default());
//!
//! fn leave_the_level(mut commands: Commands) {
//!     // The scene is about to despawn its own sound sources, so there is no
//!     // per-sound list to fade — take the whole mix down instead.
//!     commands.trigger(FadeMix::out(Duration::from_millis(800)));
//! }
//!
//! /// Whoever faded out owns fading back: the mix stays silent, and the
//! /// config-driven master write stands down, until this fires.
//! fn enter_the_next_level(mut commands: Commands) {
//!     commands.trigger(FadeMix::back(Duration::from_millis(800)));
//! }
//! # app.add_systems(Update, (leave_the_level, enter_the_next_level).chain());
//! # app.update();
//! ```

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
/// linear for short fades, `Volume::Decibels` for music-length ones (half a
/// second or longer).
///
/// In decibel space a `linear` of `0.0` becomes the −60 dB floor rather
/// than true silence — −∞ dB has no finite ramp to it, and `bevy_seedling`
/// clamps dB interpolation to the same floor. A music-length fade-out
/// therefore idles at linear ~`0.001`, inaudible on any real mix, until
/// faded back or written over directly.
#[must_use]
pub fn fade_target(linear: f32, duration: Duration) -> Volume {
    if duration.as_secs_f32() < DB_SPACE_FADE_MIN_SECS {
        return Volume::Linear(linear);
    }
    let db = Volume::Linear(linear).decibels();
    Volume::Decibels(db.max(DB_FADE_SILENCE_DB))
}

/// Where a [`FadeMix`] points the main bus.
#[derive(Reflect, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixLevel {
    /// Silence — or as close as the fade gets: a music-length fade runs in
    /// decibel space and bottoms out at the −60 dB interpolation floor
    /// (linear ~`0.001`) rather than a true zero. See [`fade_target`].
    Silent,
    /// Whatever the host's [`AudioConfig`] makes the mix when the fade is
    /// fired.
    Full,
}

/// Where the mix was last pointed by a [`FadeMix`].
///
/// Maintained by [`MixFadePlugin`]'s observer and consulted by the
/// config-driven master-volume system: while the mix is pointed at
/// [`MixLevel::Silent`] — already there, or still fading — a master-volume
/// or mute change must not snap the bus back to full, so that write stands
/// down until [`FadeMix::back`] re-engages the config's level. A host
/// driving the main bus itself can read this to respect an engaged fade the
/// same way.
#[derive(Resource, Reflect, Debug, Clone, Copy, PartialEq, Eq)]
#[reflect(Resource)]
pub struct MixFadeState {
    /// Where the last [`FadeMix`] pointed the bus; [`MixLevel::Full`] before
    /// any fade fires.
    pub target: MixLevel,
}

impl Default for MixFadeState {
    fn default() -> Self {
        Self {
            target: MixLevel::Full,
        }
    }
}

/// Fades the whole mix to [`MixLevel`] over `duration`.
///
/// Trigger it as an event; [`MixFadePlugin`] installs the observer.
#[derive(Event, Reflect, Debug, Clone, Copy)]
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
    mut state: ResMut<MixFadeState>,
    bus: Single<(&VolumeNode, &mut AudioEvents), With<MainBus>>,
) {
    let fade = *trigger.event();
    state.target = fade.to;
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

/// Installs the [`FadeMix`] observer and the [`MixFadeState`] it maintains,
/// reading the restore level from `Conf`.
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
        app.register_type::<MixLevel>();
        app.register_type::<MixFadeState>();
        app.register_type::<FadeMix>();
        app.init_resource::<Conf>();
        app.init_resource::<MixFadeState>();
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
    fn the_mix_starts_pointed_at_full() {
        assert_eq!(MixFadeState::default().target, MixLevel::Full);
    }

    #[test]
    fn constructors_point_the_right_way() {
        let out = FadeMix::out(Duration::from_secs(1));
        assert_eq!(out.to, MixLevel::Silent);
        let back = FadeMix::back(Duration::from_secs(1));
        assert_eq!(back.to, MixLevel::Full);
    }
}
