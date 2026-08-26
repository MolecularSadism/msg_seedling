use core::time::Duration;

use bevy::prelude::*;
use bevy_seedling::prelude::*;
// Explicit: `bevy::prelude` exports a `PlaybackSettings` of its own and the
// two globs would otherwise resolve to it.
use bevy_seedling::prelude::PlaybackSettings;

use crate::baseline::{BasePitch, BaseVolume};
use crate::fade::FadeOutAudio;
use crate::messages::{FadeAudio, PlayAudio, StopAudio};
#[cfg(feature = "rand")]
use crate::randomization::{DefaultRandomization, deviate, resolve_randomization};
use crate::traits::AudioCategory;

/// Spawns one sound for a [`PlayAudio`] request, with its per-sound
/// baselines already resolved.
///
/// The two baselines are the whole point of routing every spawn through here:
/// the volume node is seeded with `category volume × base`, but the `base`
/// itself is recorded on the entity so [`update_category_volumes`] and
/// [`apply_sound_damping`] can re-derive that product later instead of
/// overwriting it. See [`baseline`](crate::baseline).
///
/// [`update_category_volumes`]: crate::audio_systems::update_category_volumes
/// [`apply_sound_damping`]: crate::damping::apply_sound_damping
fn spawn_requested_sound<C: AudioCategory>(
    commands: &mut Commands,
    msg: &PlayAudio<C>,
    config: &C::Config,
    base: BaseVolume,
    pitch: BasePitch,
) {
    let mut player = SamplePlayer::new(msg.handle.clone());
    if msg.looping {
        player = player.looping();
    }

    let mut entity = commands.spawn((
        player,
        msg.category,
        base,
        pitch,
        PlaybackSettings {
            speed: pitch.0.into(),
            ..Default::default()
        },
        sample_effects![VolumeNode::from_linear(
            base.resolve(msg.category.volume(config))
        )],
        Name::new(format!("{:?}", msg.category)),
    ));

    // A request with a parent or a position is an event somewhere in the
    // world and is spatialized; everything else plays flat.
    if let Some(parent) = msg.parent {
        entity.insert((SpatialPool, ChildOf(parent), Transform::default()));
    } else if let Some(position) = msg.position {
        entity.insert((SpatialPool, Transform::from_translation(position.as_vec3())));
    } else {
        entity.insert(DefaultPool);
    }
}

/// System that handles [`PlayAudio`] messages by spawning seedling `SamplePlayer` entities.
///
/// Both randomization axes are drawn here, from one system-local RNG, and
/// baked into the entity's [`BaseVolume`]/[`BasePitch`] rather than into the
/// volume node and playback speed alone. Drawing the speed here rather than
/// deferring to seedling's `RandomPitch` (which applies in `Last`, a frame
/// later) is what makes the pitch baseline knowable at spawn — a
/// [`SoundDampingField`](crate::SoundDampingField) bends a randomized
/// footstep down from *its* pitch, and could not if the baseline arrived
/// after the first damped frame.
#[cfg(feature = "rand")]
pub fn handle_play_audio<C: AudioCategory>(
    mut commands: Commands,
    mut rng: Local<crate::randomization::AudioRng>,
    defaults: Res<DefaultRandomization>,
    config: Res<C::Config>,
    mut messages: MessageReader<PlayAudio<C>>,
) {
    for msg in messages.read() {
        let (vol_rand, spd_rand) = resolve_randomization(msg.randomization, &defaults);
        let base = BaseVolume::new(deviate(&mut rng.0, msg.volume, vol_rand));
        let pitch = BasePitch::new(deviate(&mut rng.0, 1.0, spd_rand));
        spawn_requested_sound(&mut commands, msg, &config, base, pitch);
    }
}

/// System that handles [`PlayAudio`] messages without randomization (no `rand` feature).
///
/// Every sound still carries its [`BaseVolume`]/[`BasePitch`]; only the
/// deviation is missing, so the baselines are the request's own volume and an
/// unbent pitch.
#[cfg(not(feature = "rand"))]
pub fn handle_play_audio<C: AudioCategory>(
    mut commands: Commands,
    config: Res<C::Config>,
    mut messages: MessageReader<PlayAudio<C>>,
) {
    for msg in messages.read() {
        let base = BaseVolume::new(msg.volume);
        spawn_requested_sound(&mut commands, msg, &config, base, BasePitch::default());
    }
}

/// System that handles [`StopAudio`] messages by despawning matching entities.
pub fn handle_stop_audio<C: AudioCategory>(
    mut commands: Commands,
    mut messages: MessageReader<StopAudio<C>>,
    categorized: Query<(Entity, &C)>,
    all_audio: Query<Entity, With<SamplePlayer>>,
) {
    for msg in messages.read() {
        match msg.category {
            Some(category) => {
                for (entity, cat) in &categorized {
                    if *cat == category {
                        commands.entity(entity).despawn();
                    }
                }
            }
            None => {
                for entity in &all_audio {
                    commands.entity(entity).despawn();
                }
            }
        }
    }
}

/// System that handles [`FadeAudio`] messages by fading matching sounds to
/// silence through [`FadeOutAudio`](crate::fade::FadeOutAudio).
///
/// The fade component (rather than a bare `fade_to`) marks the sound for
/// the crate's other volume writers: the config-driven rewrites and
/// [`apply_sound_damping`](crate::damping::apply_sound_damping) leave a
/// fading sound's volume node alone instead of fighting the in-flight ramp.
/// The entity is kept once the fade completes, matching the message's
/// fade-to-silence-in-place semantics.
pub fn handle_fade_audio<C: AudioCategory>(
    mut commands: Commands,
    mut messages: MessageReader<FadeAudio<C>>,
    categorized: Query<(Entity, &C), With<SampleEffects>>,
) {
    for msg in messages.read() {
        let duration = Duration::try_from_secs_f32(msg.duration_secs).unwrap_or(Duration::ZERO);
        for (entity, cat) in &categorized {
            if *cat == msg.category {
                commands
                    .entity(entity)
                    .insert(FadeOutAudio::new(duration).keep_entity());
            }
        }
    }
}
