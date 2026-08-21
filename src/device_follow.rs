//! Keeps the audio stream bound to the operating system's default output
//! device.
//!
//! `cpal` opens one concrete device and never rebinds it. When the user
//! switches the system default output (or plugs in headphones that leave the
//! old device alive), the stream keeps playing on the old device.
//! `bevy_seedling` only restarts the stream when it *errors* — that is, when
//! the current device disappears entirely — so a plain default-device change
//! goes unnoticed.
//!
//! This module polls the default output device on an interval and, when it
//! changes while the stream config does not pin a specific device, asks
//! `bevy_seedling` to restart the stream. With no pinned device, the restarted
//! stream opens whatever the OS now reports as the default output.
//!
//! This is a standalone plugin, independent of [`MsgSeedlingPlugin`] and of
//! the audio category type: add it once, however many category types the app
//! uses.
//!
//! ```
//! # use bevy::prelude::*;
//! # let mut app = App::new();
//! # app.add_plugins(MinimalPlugins);
//! app.add_plugins(msg_seedling::device_follow::plugin);
//! # app.update();
//! ```
//!
//! [`MsgSeedlingPlugin`]: crate::MsgSeedlingPlugin

use bevy::prelude::*;
use bevy_seedling::configuration::{FetchAudioIoEvent, RestartAudioEvent};
use bevy_seedling::context::{
    AudioContext, AudioStreamConfig, StreamRestartEvent, StreamStartEvent,
};
use core::time::Duration;

/// Adds OS default-output-device following.
///
/// Insert or remove [`FollowDefaultAudioDevice`] to toggle the behavior at
/// runtime; the resource is inserted here with its default poll interval.
pub fn plugin(app: &mut App) {
    app.register_type::<FollowDefaultAudioDevice>();
    app.init_resource::<FollowDefaultAudioDevice>();
    app.init_resource::<FollowDefaultState>();
    app.add_systems(
        Update,
        follow_default_device.run_if(
            resource_exists::<FollowDefaultAudioDevice>
                .and(resource_exists::<AudioContext>)
                .and(resource_exists::<AudioStreamConfig>),
        ),
    );
    app.add_observer(resync_on_stream_start);
    app.add_observer(resync_on_stream_restart);
}

/// Enables following the operating system's default audio output device.
///
/// While this resource exists (and [`AudioStreamConfig`] does not pin a
/// specific output device), the default output device is polled every
/// [`poll_interval`](Self::poll_interval); when it changes, the audio stream
/// restarts on the new default. Remove the resource to stop following.
#[derive(Resource, Debug, Clone, Reflect)]
#[reflect(Resource)]
pub struct FollowDefaultAudioDevice {
    /// How often to check which output device the OS considers the default.
    ///
    /// Each check enumerates the host's audio devices, so keep this in the
    /// hundreds of milliseconds or above. Changing it retimes the poll on the
    /// next run.
    pub poll_interval: Duration,
}

impl Default for FollowDefaultAudioDevice {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
        }
    }
}

/// Internal polling state: the poll timer and the last-observed default
/// output device id.
#[derive(Resource, Debug)]
struct FollowDefaultState {
    timer: Timer,
    last_default: Option<String>,
}

impl Default for FollowDefaultState {
    fn default() -> Self {
        Self {
            timer: Timer::new(
                FollowDefaultAudioDevice::default().poll_interval,
                TimerMode::Repeating,
            ),
            last_default: None,
        }
    }
}

/// The id of the device the OS currently reports as the default output.
///
/// Returns `None` when enumeration fails or yields no devices.
fn current_default(context: &mut AudioContext) -> Option<String> {
    context.with(|context| {
        context
            .output_devices_simple()
            .into_iter()
            .next()
            .map(|device| device.id)
    })
}

/// Polls the OS default output device and restarts the stream when it moves.
fn follow_default_device(
    time: Res<Time>,
    settings: Res<FollowDefaultAudioDevice>,
    mut state: ResMut<FollowDefaultState>,
    mut context: ResMut<AudioContext>,
    config: Res<AudioStreamConfig>,
    mut commands: Commands,
) {
    let state = &mut *state;

    if state.timer.duration() != settings.poll_interval {
        state.timer.set_duration(settings.poll_interval);
    }
    if !state.timer.tick(time.delta()).just_finished() {
        return;
    }

    let restart_needed = restart_needed(
        config.0.output.device_id.is_some(),
        &mut state.last_default,
        || current_default(&mut context),
    );

    if restart_needed {
        bevy::log::info!("System default audio output changed; restarting audio stream.");
        // Refresh the device entities first: `RestartAudioEvent`'s observer
        // reads them to re-select a device when the configured one is gone.
        commands.trigger(FetchAudioIoEvent);
        commands.trigger(RestartAudioEvent);
    }
}

/// Whether the OS default having moved warrants a stream restart.
///
/// `enumerate` yields the id the OS currently reports as its default output.
/// It is only called when no device is pinned, so a pinned configuration
/// costs no device enumeration at all.
///
/// A pinned device is a deliberate choice and is never overridden: the cache
/// is left untouched while one is set, and the resync observers reseed it
/// whenever the stream next starts or restarts.
fn restart_needed(
    pinned: bool,
    last_default: &mut Option<String>,
    enumerate: impl FnOnce() -> Option<String>,
) -> bool {
    if pinned {
        return false;
    }
    default_changed(last_default, enumerate())
}

/// Records the default device the stream just started on, so a restart
/// initiated elsewhere (initial startup, or `bevy_seedling`'s own recovery
/// after a device disappears) is not followed by a second, redundant restart
/// from the poller.
fn resync_on_stream_start(
    _: On<StreamStartEvent>,
    mut state: ResMut<FollowDefaultState>,
    mut context: ResMut<AudioContext>,
) {
    state.last_default = current_default(&mut context);
}

/// See [`resync_on_stream_start`].
fn resync_on_stream_restart(
    _: On<StreamRestartEvent>,
    mut state: ResMut<FollowDefaultState>,
    mut context: ResMut<AudioContext>,
) {
    state.last_default = current_default(&mut context);
}

/// Compares the last-observed default device id against the current one.
///
/// Returns `true` only when a previously-observed id is replaced by a
/// different one. The first observation seeds the cache, and enumeration
/// failures (`None`) leave it untouched, so neither produces a restart.
fn default_changed(last: &mut Option<String>, current: Option<String>) -> bool {
    let Some(current) = current else {
        return false;
    };
    match last {
        Some(previous) if *previous != current => {
            *last = Some(current);
            true
        }
        Some(_) => false,
        None => {
            *last = Some(current);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    #[test]
    fn first_observation_seeds_without_restart() {
        let mut last = None;
        assert!(!default_changed(&mut last, Some("speakers".into())));
        assert_eq!(last.as_deref(), Some("speakers"));
    }

    #[test]
    fn unchanged_default_does_not_restart() {
        let mut last = Some(String::from("speakers"));
        assert!(!default_changed(&mut last, Some("speakers".into())));
        assert_eq!(last.as_deref(), Some("speakers"));
    }

    #[test]
    fn changed_default_restarts_and_updates_cache() {
        let mut last = Some(String::from("speakers"));
        assert!(default_changed(&mut last, Some("headphones".into())));
        assert_eq!(last.as_deref(), Some("headphones"));
    }

    #[test]
    fn enumeration_failure_keeps_cache_and_does_not_restart() {
        let mut last = Some(String::from("speakers"));
        assert!(!default_changed(&mut last, None));
        assert_eq!(last.as_deref(), Some("speakers"));

        let mut empty = None;
        assert!(!default_changed(&mut empty, None));
        assert_eq!(empty, None);
    }

    #[test]
    fn a_pinned_device_is_never_overridden() {
        let mut last = Some(String::from("speakers"));
        let enumerated = Cell::new(false);

        let restart = restart_needed(true, &mut last, || {
            enumerated.set(true);
            Some(String::from("headphones"))
        });

        assert!(!restart, "a pinned device must not trigger a restart");
        assert!(
            !enumerated.get(),
            "a pinned device must not cost a device enumeration"
        );
        assert_eq!(
            last.as_deref(),
            Some("speakers"),
            "the cache is left for the resync observers to reseed"
        );
    }

    #[test]
    fn an_unpinned_device_follows_the_os_default() {
        let mut last = Some(String::from("speakers"));
        let restart = restart_needed(false, &mut last, || Some(String::from("headphones")));

        assert!(restart);
        assert_eq!(last.as_deref(), Some("headphones"));
    }

    #[test]
    fn the_poll_timer_repeats_at_the_configured_interval() {
        let mut state = FollowDefaultState::default();
        assert_eq!(state.timer.mode(), TimerMode::Repeating);

        state.timer.set_duration(Duration::from_millis(500));
        assert!(!state.timer.tick(Duration::from_millis(300)).just_finished());
        assert!(state.timer.tick(Duration::from_millis(300)).just_finished());
        assert!(!state.timer.tick(Duration::from_millis(100)).just_finished());
        assert!(state.timer.tick(Duration::from_millis(500)).just_finished());
    }
}
