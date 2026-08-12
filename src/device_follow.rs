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
//! changes while the stream config does not pin a specific device, forces a
//! stream restart. With no pinned device, the restarted stream opens whatever
//! the OS now reports as the default output.

use bevy::prelude::*;
use bevy_seedling::configuration::FetchAudioIoEvent;
use bevy_seedling::context::{
    AudioContext, AudioStreamConfig, StreamRestartEvent, StreamStartEvent,
};
use core::time::Duration;

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
    /// hundreds of milliseconds or above.
    pub poll_interval: Duration,
}

impl Default for FollowDefaultAudioDevice {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
        }
    }
}

/// Internal polling state: accumulated time since the last check and the
/// last-observed default output device id.
#[derive(Resource, Debug, Default)]
pub(crate) struct FollowDefaultState {
    elapsed: Duration,
    last_default: Option<String>,
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
pub(crate) fn follow_default_device(
    time: Res<Time>,
    settings: Res<FollowDefaultAudioDevice>,
    mut state: ResMut<FollowDefaultState>,
    mut context: ResMut<AudioContext>,
    mut config: ResMut<AudioStreamConfig>,
    mut commands: Commands,
) {
    state.elapsed += time.delta();
    if state.elapsed < settings.poll_interval {
        return;
    }
    state.elapsed = Duration::ZERO;

    // A pinned device is an explicit choice; never override it.
    if config.0.output.device_id.is_some() {
        return;
    }

    let current = current_default(&mut context);
    if default_changed(&mut state.last_default, current) {
        bevy::log::info!("System default audio output changed; restarting audio stream.");
        commands.trigger(FetchAudioIoEvent);
        // Touching the config restarts the stream; with no pinned device the
        // new stream opens the OS's current default output.
        config.set_changed();
    }
}

/// Records the default device the stream just started on, so a restart
/// initiated elsewhere (initial startup, or `bevy_seedling`'s own recovery
/// after a device disappears) is not followed by a second, redundant restart
/// from the poller.
pub(crate) fn resync_on_stream_start(
    _: On<StreamStartEvent>,
    mut state: ResMut<FollowDefaultState>,
    mut context: ResMut<AudioContext>,
) {
    state.last_default = current_default(&mut context);
}

/// See [`resync_on_stream_start`].
pub(crate) fn resync_on_stream_restart(
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
}
