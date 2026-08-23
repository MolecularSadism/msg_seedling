use core::time::Duration;

use bevy::prelude::*;
use bevy_seedling::prelude::*;

use crate::fade::FadeOutAudio;
use crate::messages::{FadeAudio, PlayAudio, StopAudio};
use crate::randomization::{DefaultRandomization, resolve_randomization};
use crate::traits::AudioCategory;

/// System that handles [`PlayAudio`] messages by spawning seedling `SamplePlayer` entities.
#[cfg(feature = "rand")]
pub fn handle_play_audio<C: AudioCategory>(
    mut commands: Commands,
    mut rng: Local<crate::randomization::AudioRng>,
    defaults: Res<DefaultRandomization>,
    config: Res<C::Config>,
    mut messages: MessageReader<PlayAudio<C>>,
) {
    use rand::Rng;

    for msg in messages.read() {
        let (vol_rand, spd_rand) = resolve_randomization(msg.randomization, &defaults);

        // Compute category volume
        let category_volume = msg.category.volume(&config) * msg.volume;

        // Resolve volume with optional randomization
        let final_volume = match vol_rand {
            Some(dev) if dev > 0.0 => {
                let min = category_volume * (1.0 - dev);
                let max = category_volume * (1.0 + dev);
                rng.0.random_range(min..=max)
            }
            _ => category_volume,
        };

        // Build sample player
        let mut player = SamplePlayer::new(msg.handle.clone());
        if msg.looping {
            player = player.looping();
        }

        let is_spatial = msg.parent.is_some() || msg.position.is_some();

        // Spawn entity with appropriate pool
        let mut entity = if is_spatial {
            commands.spawn((
                player,
                SpatialPool,
                msg.category,
                sample_effects![VolumeNode::from_linear(final_volume)],
                Name::new(format!("{:?}", msg.category)),
            ))
        } else {
            commands.spawn((
                player,
                DefaultPool,
                msg.category,
                sample_effects![VolumeNode::from_linear(final_volume)],
                Name::new(format!("{:?}", msg.category)),
            ))
        };

        // Speed randomization via seedling's RandomPitch
        if let Some(dev) = spd_rand
            && dev > 0.0
        {
            entity.insert(RandomPitch::new(dev as f64));
        }

        // Spatial positioning
        if let Some(parent) = msg.parent {
            entity.insert((ChildOf(parent), Transform::default()));
        } else if let Some(position) = msg.position {
            entity.insert(Transform::from_translation(position.as_vec3()));
        }
    }
}

/// System that handles [`PlayAudio`] messages without randomization (no `rand` feature).
#[cfg(not(feature = "rand"))]
pub fn handle_play_audio<C: AudioCategory>(
    mut commands: Commands,
    config: Res<C::Config>,
    mut messages: MessageReader<PlayAudio<C>>,
) {
    for msg in messages.read() {
        let category_volume = msg.category.volume(&config) * msg.volume;

        let mut player = SamplePlayer::new(msg.handle.clone());
        if msg.looping {
            player = player.looping();
        }

        let is_spatial = msg.parent.is_some() || msg.position.is_some();

        let mut entity = if is_spatial {
            commands.spawn((
                player,
                SpatialPool,
                msg.category,
                sample_effects![VolumeNode::from_linear(category_volume)],
                Name::new(format!("{:?}", msg.category)),
            ))
        } else {
            commands.spawn((
                player,
                DefaultPool,
                msg.category,
                sample_effects![VolumeNode::from_linear(category_volume)],
                Name::new(format!("{:?}", msg.category)),
            ))
        };

        if let Some(parent) = msg.parent {
            entity.insert((ChildOf(parent), Transform::default()));
        } else if let Some(position) = msg.position {
            entity.insert(Transform::from_translation(position.as_vec3()));
        }
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
