//! Generic, category-agnostic fade-in/fade-out for any sample entity.
//!
//! Add [`FadeInAudio`]/[`FadeOutAudio`] to an entity that carries seedling's
//! `SampleEffects` (any entity spawned via [`crate::PlayAudio`], or a bare
//! `commands.spawn((SamplePlayer::new(..), sample_effects![VolumeNode::..]))`)
//! to smoothly ramp its `VolumeNode` on the audio thread instead of snapping
//! it. [`virtual_queue`](crate::virtual_queue) builds its crossfades directly
//! on these two components.

use core::time::Duration;

use bevy::prelude::*;
use bevy_seedling::prelude::*;

/// Fades an entity's `VolumeNode` in from silence to a target volume.
///
/// Removes itself once the fade has been handed to the audio thread — it
/// does not track completion, since the audio thread owns the ramp from
/// there. Retries every frame until the entity's `SampleEffects` resolve a
/// `VolumeNode`, so it is safe to insert in the same command batch as the
/// `SamplePlayer` it targets, even though seedling's pool machinery attaches
/// effects one frame later.
#[derive(Component, Reflect, Debug, Clone)]
#[reflect(Component)]
pub struct FadeInAudio {
    /// Duration of the fade.
    #[reflect(ignore)]
    pub duration: Duration,
    /// Target linear volume (0.0–1.0) to fade to.
    pub target_volume: f32,
}

impl FadeInAudio {
    /// Creates a fade-in to `target_volume` over `duration`.
    #[must_use]
    pub fn new(duration: Duration, target_volume: f32) -> Self {
        Self {
            duration,
            target_volume: target_volume.clamp(0.0, 1.0),
        }
    }
}

/// Fades an entity's `VolumeNode` out to silence, optionally despawning it
/// once the fade completes.
///
/// Like [`FadeInAudio`], the fade is handed to the audio thread as soon as
/// `SampleEffects` resolves; [`despawn_on_complete`](Self::despawn_on_complete)
/// tracks elapsed time on the game side purely to schedule that despawn —
/// the actual ramp always runs on the audio thread regardless of frame rate.
#[derive(Component, Reflect, Debug, Clone)]
#[reflect(Component)]
pub struct FadeOutAudio {
    /// Duration of the fade.
    #[reflect(ignore)]
    pub duration: Duration,
    /// Whether to despawn the entity once `duration` has elapsed.
    pub despawn_on_complete: bool,
    /// Whether the fade has been handed to the audio thread yet.
    #[reflect(ignore)]
    triggered: bool,
    /// Time elapsed since this component was added.
    #[reflect(ignore)]
    elapsed: Duration,
}

impl FadeOutAudio {
    /// Creates a fade-out to silence over `duration`, despawning the entity
    /// on completion.
    #[must_use]
    pub fn new(duration: Duration) -> Self {
        Self {
            duration,
            despawn_on_complete: true,
            triggered: false,
            elapsed: Duration::ZERO,
        }
    }

    /// Keeps the entity alive after the fade completes instead of despawning
    /// it — useful for silencing a looping sound without losing its identity.
    #[must_use]
    pub fn keep_entity(mut self) -> Self {
        self.despawn_on_complete = false;
        self
    }
}

/// Marker resource: guards [`plugin`] against double-registration.
///
/// [`crate::virtual_queue::VirtualVoiceQueuePlugin`] calls this function
/// directly (not through `app.add_plugins`, which would panic on a second
/// call for a second category type) — this resource makes repeated calls a
/// safe no-op instead, whether they come from multiple category types or a
/// user who also added `fade::plugin` explicitly.
#[derive(Resource)]
struct FadeSystemsRegistered;

/// Registers [`FadeInAudio`]/[`FadeOutAudio`] and their driving systems.
///
/// Safe to call more than once — only the first call has any effect. Add it
/// directly if you want fades without [`crate::virtual_queue`]; otherwise
/// [`VirtualVoiceQueuePlugin`](crate::virtual_queue::VirtualVoiceQueuePlugin)
/// pulls it in automatically.
pub fn plugin(app: &mut App) {
    if app.world().contains_resource::<FadeSystemsRegistered>() {
        return;
    }
    app.insert_resource(FadeSystemsRegistered);
    app.register_type::<FadeInAudio>();
    app.register_type::<FadeOutAudio>();
    app.add_systems(Update, (fade_in_audio_system, fade_out_audio_system));
}

/// Hands [`FadeInAudio`] to the audio thread and removes the component.
fn fade_in_audio_system(
    mut commands: Commands,
    mut query: Query<(Entity, &FadeInAudio, &SampleEffects)>,
    mut volume_nodes: Query<(&VolumeNode, &mut AudioEvents)>,
) {
    for (entity, fade, effects) in &mut query {
        let Ok((volume_node, mut events)) = volume_nodes.get_effect_mut(effects) else {
            continue;
        };
        volume_node.fade_to(
            Volume::Linear(fade.target_volume),
            DurationSeconds(fade.duration.as_secs_f64()),
            &mut events,
        );
        commands.entity(entity).remove::<FadeInAudio>();
    }
}

/// Hands [`FadeOutAudio`] to the audio thread on first sight, then despawns
/// the entity once its duration has elapsed (if configured to).
fn fade_out_audio_system(
    mut commands: Commands,
    time: Res<Time>,
    mut query: Query<(Entity, &mut FadeOutAudio, &SampleEffects)>,
    mut volume_nodes: Query<(&VolumeNode, &mut AudioEvents)>,
) {
    let delta = time.delta();

    for (entity, mut fade, effects) in &mut query {
        if !fade.triggered
            && let Ok((volume_node, mut events)) = volume_nodes.get_effect_mut(effects)
        {
            volume_node.fade_to(
                Volume::SILENT,
                DurationSeconds(fade.duration.as_secs_f64()),
                &mut events,
            );
            fade.triggered = true;
        }

        fade.elapsed += delta;

        if fade.elapsed >= fade.duration && fade.despawn_on_complete {
            commands.entity(entity).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_in_clamps_target_volume() {
        let fade = FadeInAudio::new(Duration::from_millis(100), 1.5);
        assert!((fade.target_volume - 1.0).abs() < f32::EPSILON);

        let fade = FadeInAudio::new(Duration::from_millis(100), -0.5);
        assert!((fade.target_volume - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn fade_out_despawns_by_default() {
        let fade = FadeOutAudio::new(Duration::from_millis(50));
        assert!(fade.despawn_on_complete);
    }

    #[test]
    fn fade_out_keep_entity_disables_despawn() {
        let fade = FadeOutAudio::new(Duration::from_millis(50)).keep_entity();
        assert!(!fade.despawn_on_complete);
    }

    #[test]
    fn fade_out_system_despawns_after_duration() {
        use msg_testing::{AppTesting, physics_app};

        let mut app = physics_app();
        app.add_systems(Update, fade_out_audio_system);

        // The outer query requires `SampleEffects`, which only a relationship
        // spawn (not a bare `Default`) can produce — `sample_effects!` links
        // a real (if audio-backend-less) effect child, same as production
        // code. `AudioEvents` never resolves without a running audio
        // context, so the trigger itself silently no-ops here; this test
        // exercises only the elapsed-time despawn scheduling.
        let entity = app
            .world_mut()
            .spawn((
                FadeOutAudio::new(Duration::from_millis(10)),
                sample_effects![VolumeNode::from_linear(1.0)],
            ))
            .id();

        // `physics_app()` steps `Time` by one fixed timestep (~15.6ms)
        // per `update()`, already past the 10ms fade duration.
        app.update_n(1);
        assert!(app.world().get_entity(entity).is_err());
    }
}
