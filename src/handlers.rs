use bevy::prelude::*;
use bevy_seedling::prelude::*;

use crate::messages::{FadeAudio, PlayAudio, StopAudio};
use crate::randomization::{resolve_randomization, DefaultRandomization};
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
    #[allow(deprecated)]
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

/// System that handles [`FadeAudio`] messages using seedling's `VolumeFade`.
pub fn handle_fade_audio<C: AudioCategory>(
    mut messages: MessageReader<FadeAudio<C>>,
    categorized: Query<(&C, &SampleEffects)>,
    mut volume_nodes: Query<(&VolumeNode, &mut AudioEvents)>,
) {
    for msg in messages.read() {
        for (cat, effects) in &categorized {
            if *cat == msg.category
                && let Ok((volume_node, mut events)) = volume_nodes.get_effect_mut(effects)
            {
                volume_node.fade_to(
                    Volume::SILENT,
                    DurationSeconds(msg.duration_secs as f64),
                    &mut events,
                );
            }
        }
    }
}
