//! Sidechain-style ducking: high-priority cues dip the routine mix.
//!
//! Raising a crucial cue's own gain to make it land would feed the very
//! saturation that buries it. A mix makes a cue land the other way around:
//! the moment it starts, everything routine steps back a few decibels and
//! returns once the cue has had its say.
//!
//! One [`DuckingEnvelope`] serves the whole mix. The host triggers it when a
//! qualifying sound actually spawns (a request dropped by a budget ducks
//! nothing), it attacks fast, holds briefly, and releases slow — the classic
//! sidechain shape. The envelope itself never touches a volume node: fold
//! [`DuckingEnvelope::gain_for`] into whichever per-frame volume write the
//! host (or [`crate::damping::apply_sound_damping`]) already owns, because
//! two systems writing the same nodes on alternate frames would flicker.
//!
//! *Which* sounds duck and *which* spawns trigger the duck are host policy:
//! mark duckable sounds with the [`Ducks`] component at spawn (a category and
//! priority are fixed by then, so the policy is a spawn-time decision), and
//! call [`DuckingEnvelope::trigger`] from the host's own qualifying-spawn
//! path.
//!
//! ## Example
//!
//! ```
//! # use bevy::prelude::*;
//! # use msg_seedling::prelude::*;
//! # use bevy_seedling::prelude::*;
//! # #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
//! # #[reflect(Component)]
//! # enum Sound { #[default] Sfx }
//! # #[derive(Resource, Clone, Default)]
//! # struct GameAudioConfig;
//! # impl AudioConfig for GameAudioConfig {
//! #     fn master_volume(&self) -> f32 { 1.0 }
//! # }
//! # impl AudioCategory for Sound {
//! #     type Config = GameAudioConfig;
//! #     fn volume(&self, _config: &GameAudioConfig) -> f32 { 1.0 }
//! # }
//! # let mut app = App::new();
//! # app.add_plugins(MinimalPlugins);
//! // `DampingPlugin` pulls the envelope in and folds it into its volume
//! // write; `ducking::plugin` alone is enough if you write your own.
//! app.add_plugins(DampingPlugin::<Sound>::default());
//!
//! /// The routine bed: marked once, at spawn.
//! fn play_ambience(mut commands: Commands) {
//!     commands.spawn((
//!         SamplePlayer::new(Handle::default()).looping(),
//!         Sound::Sfx,
//!         Ducks,
//!     ));
//! }
//!
//! /// The cue that has to land: it does not carry `Ducks` itself, and it
//! /// steps the rest of the mix back on the frame it spawns.
//! fn play_objective_cue(mut commands: Commands, mut duck: ResMut<DuckingEnvelope>) {
//!     commands.spawn((SamplePlayer::new(Handle::default()), Sound::Sfx));
//!     duck.trigger();
//! }
//! # app.add_systems(Update, (play_ambience, play_objective_cue));
//! # app.update();
//! ```

use bevy::prelude::*;

/// Floor for the envelope's timing fields when read: a zero, negative, or
/// NaN attack, hold, or release still yields a finite rate that snaps to its
/// target within a frame instead of an inf/NaN one.
const MIN_SHAPE_SECS: f32 = 1e-4;

/// The one mix-wide ducking envelope.
///
/// `1.0` at rest; pulled toward [`Self::ducked_gain`] while a cue holds it.
/// The shape parameters are public so a host can tune the sidechain; the
/// defaults are a felt-but-safe −6 dB dip with a fast attack and slow
/// release. Because they are freely writable, the shape fields are
/// sanitized where they are read ([`Self::trigger`] and [`Self::tick`])
/// rather than at assignment: no configuration can push the gain outside
/// `0.0..=1.0` or keep [`Self::is_idle`] from ever returning `true`.
#[derive(Resource, Reflect, Debug, Clone, Copy, PartialEq)]
#[reflect(Resource)]
pub struct DuckingEnvelope {
    /// Current linear gain applied to duckable sounds.
    gain: f32,
    /// Seconds of full-depth hold left before the release begins.
    hold_remaining: f32,
    /// Linear gain the routine mix is pulled down to while a cue plays.
    /// Read clamped to `0.0..=1.0`.
    ///
    /// The default −6 dB (`0.5`): clearly felt, nowhere near a dropout.
    pub ducked_gain: f32,
    /// Seconds to reach [`Self::ducked_gain`] once triggered. Fast, so the
    /// cue's transient is already standing clear of the bed. Read floored
    /// just above zero — a non-positive attack snaps.
    pub attack_secs: f32,
    /// Seconds the duck holds at full depth after the most recent trigger.
    /// The hold clock starts once the attack reaches [`Self::ducked_gain`],
    /// so the full depth lasts this long regardless of the attack.
    /// Re-triggering restarts the hold, so a sustained barrage keeps the bed
    /// down. Read floored just above zero — a non-positive hold still ducks
    /// for a single instant.
    pub hold_secs: f32,
    /// Seconds to recover to unity after the hold lapses. Slow enough that
    /// the bed swells back instead of popping. Read floored just above zero
    /// — a non-positive release snaps.
    pub release_secs: f32,
}

impl Default for DuckingEnvelope {
    fn default() -> Self {
        Self {
            gain: 1.0,
            hold_remaining: 0.0,
            ducked_gain: 0.5,
            attack_secs: 0.04,
            hold_secs: 0.25,
            release_secs: 0.45,
        }
    }
}

impl DuckingEnvelope {
    /// Starts (or extends) the duck.
    pub fn trigger(&mut self) {
        self.hold_remaining = self.hold_secs.max(MIN_SHAPE_SECS);
    }

    /// Whether the envelope is at rest and leaving every sound alone.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.hold_remaining <= 0.0 && self.gain >= 1.0
    }

    /// The envelope's current linear gain, e.g. for display in a mixer panel.
    #[must_use]
    pub fn gain(&self) -> f32 {
        self.gain
    }

    /// The gain this envelope applies to one sound: the current gain when the
    /// sound ducks (see [`Ducks`]), unity when it stands clear.
    #[must_use]
    pub fn gain_for(&self, ducks: bool) -> f32 {
        if ducks { self.gain } else { 1.0 }
    }

    /// Advances the envelope by `delta_secs`.
    pub fn tick(&mut self, delta_secs: f32) {
        let ducked = self.ducked_gain.clamp(0.0, 1.0);
        if self.hold_remaining > 0.0 {
            if self.gain > ducked {
                // Attack: the hold clock waits until full depth is reached.
                let attack_rate = (1.0 - ducked) / self.attack_secs.max(MIN_SHAPE_SECS);
                self.gain = (self.gain - attack_rate * delta_secs).max(ducked);
            } else {
                self.hold_remaining -= delta_secs;
            }
        } else {
            let release_rate = (1.0 - ducked) / self.release_secs.max(MIN_SHAPE_SECS);
            self.gain = (self.gain + release_rate * delta_secs).min(1.0);
        }
    }
}

/// Marks a sound that steps back while the [`DuckingEnvelope`] is engaged.
///
/// Host policy, decided at spawn: routine world sounds (ambient beds, low
/// priority one-shots) carry it; the feedback cues themselves, music, and
/// interface audio do not. [`crate::damping::apply_sound_damping`] folds the
/// envelope into the volume of marked sounds; a host writing its own volume
/// nodes reads [`DuckingEnvelope::gain_for`] the same way.
#[derive(Component, Reflect, Debug, Default, Clone, Copy)]
#[reflect(Component)]
pub struct Ducks;

/// Advances the envelope each frame; a no-op (and no change-detection churn)
/// while idle. Schedule between the host's spawn path and its volume write so
/// a trigger's attack starts on the very frame its cue spawns.
pub fn tick_ducking_envelope(time: Res<Time>, mut duck: ResMut<DuckingEnvelope>) {
    if duck.is_idle() {
        return;
    }
    duck.tick(time.delta_secs());
}

/// Marker resource: guards [`plugin`] against double-registration.
#[derive(Resource)]
struct DuckingRegistered;

/// Registers the [`DuckingEnvelope`] resource, the [`Ducks`] marker, and the
/// per-frame tick. Safe to call more than once — only the first call has any
/// effect.
pub fn plugin(app: &mut App) {
    if app.world().contains_resource::<DuckingRegistered>() {
        return;
    }
    app.insert_resource(DuckingRegistered);
    app.register_type::<DuckingEnvelope>();
    app.register_type::<Ducks>();
    app.init_resource::<DuckingEnvelope>();
    app.add_systems(Update, tick_ducking_envelope);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trigger_attacks_holds_and_releases() {
        let mut duck = DuckingEnvelope::default();
        assert!(duck.is_idle());

        duck.trigger();
        duck.tick(duck.attack_secs);
        assert_eq!(
            duck.gain, duck.ducked_gain,
            "the attack bottoms out on time"
        );
        assert_eq!(
            duck.hold_remaining, duck.hold_secs,
            "the hold clock starts only at full depth"
        );

        // Holding: still at depth just before the hold lapses.
        duck.tick(duck.hold_secs - 0.01);
        assert_eq!(duck.gain, duck.ducked_gain);
        assert!(!duck.is_idle());

        // Releasing: recovered to unity after the release window.
        duck.tick(0.02);
        duck.tick(duck.release_secs);
        assert_eq!(duck.gain, 1.0);
        assert!(duck.is_idle());
    }

    #[test]
    fn a_retrigger_extends_the_hold() {
        let mut duck = DuckingEnvelope::default();
        duck.trigger();
        duck.tick(duck.hold_secs - 0.01);
        duck.trigger();
        duck.tick(duck.hold_secs - 0.01);
        assert_eq!(
            duck.gain, duck.ducked_gain,
            "a sustained barrage keeps the bed down"
        );
    }

    #[test]
    fn only_duckable_sounds_duck() {
        let mut duck = DuckingEnvelope::default();
        duck.trigger();
        duck.tick(duck.attack_secs);

        // The routine mix (host marks it with `Ducks`) steps back...
        assert_eq!(duck.gain_for(true), duck.ducked_gain);
        // ...while unmarked sounds — the cues themselves, music, interface
        // audio — hold steady.
        assert_eq!(duck.gain_for(false), 1.0);
    }

    #[test]
    fn custom_shape_parameters_drive_the_same_curve() {
        let mut duck = DuckingEnvelope {
            ducked_gain: 0.25,
            attack_secs: 0.1,
            hold_secs: 1.0,
            release_secs: 2.0,
            ..Default::default()
        };
        duck.trigger();
        duck.tick(0.1);
        assert_eq!(duck.gain, 0.25);
        duck.tick(1.0);
        duck.tick(2.0);
        assert_eq!(duck.gain, 1.0);
        assert!(duck.is_idle());
    }

    #[test]
    fn hostile_shape_values_cannot_wedge_the_envelope() {
        let mut duck = DuckingEnvelope {
            ducked_gain: 2.0,
            attack_secs: 0.0,
            hold_secs: -1.0,
            release_secs: 0.0,
            ..Default::default()
        };
        duck.trigger();
        for _ in 0..8 {
            duck.tick(0.05);
            assert!(
                (0.0..=1.0).contains(&duck.gain()),
                "gain {} escaped the unit range",
                duck.gain()
            );
        }
        assert!(duck.is_idle(), "a hostile shape must still come to rest");
    }

    #[test]
    fn a_negative_ducked_gain_bottoms_out_at_silence() {
        let mut duck = DuckingEnvelope {
            ducked_gain: -0.5,
            ..Default::default()
        };
        duck.trigger();
        duck.tick(duck.attack_secs);
        assert_eq!(duck.gain(), 0.0, "the duck stops at silence, not below");

        duck.tick(duck.hold_secs);
        duck.tick(duck.release_secs);
        assert!(duck.is_idle());
    }

    #[test]
    fn plugin_registration_is_idempotent() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        plugin(&mut app);
        plugin(&mut app);
        assert!(app.world().contains_resource::<DuckingEnvelope>());
    }
}
