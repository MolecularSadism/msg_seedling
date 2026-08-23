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

use bevy::prelude::*;

/// The one mix-wide ducking envelope.
///
/// `1.0` at rest; pulled toward [`Self::ducked_gain`] while a cue holds it.
/// The shape parameters are public so a host can tune the sidechain; the
/// defaults are a felt-but-safe −6 dB dip with a fast attack and slow
/// release.
#[derive(Resource, Reflect, Debug, Clone, Copy, PartialEq)]
#[reflect(Resource)]
pub struct DuckingEnvelope {
    /// Current linear gain applied to duckable sounds.
    gain: f32,
    /// Seconds of full-depth hold left before the release begins.
    hold_remaining: f32,
    /// Linear gain the routine mix is pulled down to while a cue plays.
    ///
    /// The default −6 dB (`0.5`): clearly felt, nowhere near a dropout.
    pub ducked_gain: f32,
    /// Seconds to reach [`Self::ducked_gain`] once triggered. Fast, so the
    /// cue's transient is already standing clear of the bed.
    pub attack_secs: f32,
    /// Seconds the duck holds at full depth after the most recent trigger.
    /// The hold clock starts once the attack reaches [`Self::ducked_gain`],
    /// so the full depth lasts this long regardless of the attack.
    /// Re-triggering restarts the hold, so a sustained barrage keeps the bed
    /// down.
    pub hold_secs: f32,
    /// Seconds to recover to unity after the hold lapses. Slow enough that
    /// the bed swells back instead of popping.
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
        self.hold_remaining = self.hold_secs;
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
        if self.hold_remaining > 0.0 {
            if self.gain > self.ducked_gain {
                // Attack: the hold clock waits until full depth is reached.
                let attack_rate = (1.0 - self.ducked_gain) / self.attack_secs;
                self.gain = (self.gain - attack_rate * delta_secs).max(self.ducked_gain);
            } else {
                self.hold_remaining -= delta_secs;
            }
        } else {
            let release_rate = (1.0 - self.ducked_gain) / self.release_secs;
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
    fn plugin_registration_is_idempotent() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        plugin(&mut app);
        plugin(&mut app);
        assert!(app.world().contains_resource::<DuckingEnvelope>());
    }
}
