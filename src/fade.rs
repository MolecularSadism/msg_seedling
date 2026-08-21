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

/// How long past its duration an untriggered fade-out waits for its
/// `SampleEffects` to resolve before completing without a ramp.
const UNRESOLVED_EFFECTS_GRACE: Duration = Duration::from_millis(500);

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
/// The fade is handed to the audio thread as soon as `SampleEffects`
/// resolves a `VolumeNode`; the game side tracks elapsed time from that
/// moment purely to schedule completion — the ramp itself always runs on the
/// audio thread regardless of frame rate. On completion the entity is
/// despawned, or with [`keep_entity`](Self::keep_entity) only this component
/// is removed. An entity whose effects never resolve (e.g. its voice was
/// stolen mid-fade) still completes once its duration plus a grace period
/// has passed since insertion, so nothing leaks.
#[derive(Component, Reflect, Debug, Clone)]
#[reflect(Component)]
pub struct FadeOutAudio {
    /// Duration of the fade.
    pub duration: Duration,
    /// Whether to despawn the entity once the fade completes.
    pub despawn_on_complete: bool,
    /// Whether the fade has been handed to the audio thread yet.
    triggered: bool,
    /// Time elapsed since the fade was handed to the audio thread.
    elapsed: Duration,
    /// Time spent waiting for `SampleEffects` to resolve.
    waiting: Duration,
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
            waiting: Duration::ZERO,
        }
    }

    /// Keeps the entity alive after the fade completes instead of despawning
    /// it — the component removes itself on completion. Useful for silencing
    /// a looping sound without losing its identity.
    #[must_use]
    pub fn keep_entity(mut self) -> Self {
        self.despawn_on_complete = false;
        self
    }

    /// Whether the fade has run its course — its duration elapsed since being
    /// handed to the audio thread, or the unresolved-effects grace exhausted —
    /// so the next fade-system run completes it.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        if self.triggered {
            self.elapsed >= self.duration
        } else {
            self.waiting >= self.duration + UNRESOLVED_EFFECTS_GRACE
        }
    }
}

/// System set containing the fade drive systems. The virtual queue's systems
/// run after this set, so a fade's completion (despawn or self-removal) is
/// applied before queue decisions read the entity's state.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FadeSystems;

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
    // Fade-out first, deterministically: if one frame both demotes and
    // re-promotes fades on related entities, the fade-in lands last.
    app.add_systems(
        Update,
        (fade_out_audio_system, fade_in_audio_system)
            .chain()
            .in_set(FadeSystems),
    );
}

/// Hands [`FadeInAudio`] to the audio thread and removes the component.
fn fade_in_audio_system(
    mut commands: Commands,
    query: Query<(Entity, &FadeInAudio, &SampleEffects)>,
    mut volume_nodes: Query<(&VolumeNode, &mut AudioEvents)>,
) {
    for (entity, fade, effects) in &query {
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

/// Hands [`FadeOutAudio`] to the audio thread on first sight, then completes
/// (despawn or self-removal) once its duration has elapsed since triggering.
fn fade_out_audio_system(
    mut commands: Commands,
    time: Res<Time>,
    mut query: Query<(Entity, &mut FadeOutAudio, Option<&SampleEffects>)>,
    mut volume_nodes: Query<(&VolumeNode, &mut AudioEvents)>,
) {
    let delta = time.delta();

    for (entity, mut fade, effects) in &mut query {
        if fade.triggered {
            fade.elapsed += delta;
            if fade.is_complete() {
                complete_fade_out(&mut commands, entity, &fade);
            }
            continue;
        }

        if let Some(effects) = effects
            && let Ok((volume_node, mut events)) = volume_nodes.get_effect_mut(effects)
        {
            volume_node.fade_to(
                Volume::SILENT,
                DurationSeconds(fade.duration.as_secs_f64()),
                &mut events,
            );
            fade.triggered = true;
            continue;
        }

        fade.waiting += delta;
        if fade.is_complete() {
            complete_fade_out(&mut commands, entity, &fade);
        }
    }
}

fn complete_fade_out(commands: &mut Commands, entity: Entity, fade: &FadeOutAudio) {
    if fade.despawn_on_complete {
        commands.entity(entity).despawn();
    } else {
        commands.entity(entity).remove::<FadeOutAudio>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use msg_testing::{AppTesting, physics_app};

    fn fade_app() -> App {
        let mut app = physics_app();
        app.add_systems(Update, fade_out_audio_system);
        app
    }

    /// Steps enough fixed frames (~15.6ms each) to pass `duration` plus the
    /// unresolved-effects grace.
    fn steps_past_grace(app: &App, duration: Duration) -> usize {
        let timestep = app
            .world()
            .resource::<Time<Fixed>>()
            .timestep()
            .as_secs_f64();
        ((duration + UNRESOLVED_EFFECTS_GRACE).as_secs_f64() / timestep).ceil() as usize + 1
    }

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
    fn untriggered_fade_out_survives_its_duration_then_hits_the_grace_deadline() {
        let mut app = fade_app();
        let duration = Duration::from_millis(10);

        // `AudioEvents` never resolves without a running audio context, so
        // the fade stays untriggered and only the fallback deadline applies.
        let entity = app
            .world_mut()
            .spawn((
                FadeOutAudio::new(duration),
                sample_effects![VolumeNode::from_linear(1.0)],
            ))
            .id();

        // Well past the 10ms duration but within the grace: still alive, so
        // a late-resolving fade would keep its full tail.
        app.update_n(3);
        assert!(app.world().get_entity(entity).is_ok());

        let steps = steps_past_grace(&app, duration);
        app.update_n(steps);
        assert!(app.world().get_entity(entity).is_err());
    }

    #[test]
    fn keep_entity_fade_out_removes_itself_and_leaves_the_entity() {
        let mut app = fade_app();
        let duration = Duration::from_millis(10);

        let entity = app
            .world_mut()
            .spawn((
                FadeOutAudio::new(duration).keep_entity(),
                sample_effects![VolumeNode::from_linear(1.0)],
            ))
            .id();

        let steps = steps_past_grace(&app, duration);
        app.update_n(steps + 3);
        assert!(app.world().get_entity(entity).is_ok());
        assert!(app.world().get::<FadeOutAudio>(entity).is_none());
    }
}
