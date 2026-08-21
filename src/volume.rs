use bevy::prelude::*;
use bevy_seedling::prelude::*;

use crate::fade::{FadeInAudio, FadeOutAudio};
use crate::traits::{AudioCategory, AudioConfig};

/// Updates the master bus volume when the audio config changes.
pub fn update_master_volume<C: AudioCategory>(
    config: Res<C::Config>,
    mut master: Single<&mut VolumeNode, With<MainBus>>,
) {
    master.volume = Volume::Linear(config.effective_volume());
}

/// Updates per-sample volume nodes when the audio config changes.
///
/// Each sample entity carries its category component and a `VolumeNode`
/// in its effects chain. When the config resource changes, this system
/// recomputes the category volume and updates the effect. Samples mid
/// [`FadeInAudio`]/[`FadeOutAudio`] are skipped — the fade owns the node
/// while present and would be clobbered by a bare volume write.
pub fn update_category_volumes<C: AudioCategory>(
    config: Res<C::Config>,
    samples: Query<(&C, &SampleEffects), (Without<FadeInAudio>, Without<FadeOutAudio>)>,
    mut volume_nodes: Query<&mut VolumeNode>,
) {
    for (category, effects) in &samples {
        let target_volume = category.volume(&config);
        if let Ok(mut vol_node) = volume_nodes.get_effect_mut(effects) {
            vol_node.volume = Volume::Linear(target_volume);
        }
    }
}
