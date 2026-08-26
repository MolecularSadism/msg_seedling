use bevy::prelude::*;
use bevy_seedling::prelude::*;

use crate::baseline::BaseVolume;
use crate::fade::{FadeInAudio, FadeOutAudio};
use crate::mix_fade::{MixFadeState, MixLevel};
use crate::traits::{AudioCategory, AudioConfig};

/// Updates the master bus volume when the audio config changes.
///
/// While a [`FadeMix`](crate::mix_fade::FadeMix) has the mix pointed at
/// [`MixLevel::Silent`] the fade owns the main bus, so the write is skipped
/// — a master-volume or mute change must not snap a silent (or still
/// fading) bus back to full. The deferred level lands with the next
/// [`FadeMix::back`](crate::mix_fade::FadeMix::back), which reads the live
/// config.
pub fn update_master_volume<C: AudioCategory>(
    config: Res<C::Config>,
    mix_fade: Option<Res<MixFadeState>>,
    mut master: Single<&mut VolumeNode, With<MainBus>>,
) {
    if mix_fade.is_some_and(|state| state.target == MixLevel::Silent) {
        return;
    }
    master.volume = Volume::Linear(config.effective_volume());
}

/// Updates per-sample volume nodes when the audio config changes.
///
/// Each sample entity carries its category component and a `VolumeNode` in
/// its effects chain. When the config resource changes, this system rewrites
/// that node as `category volume × `[`BaseVolume`] — the sound's own level
/// (its [`PlayAudio::with_volume`](crate::PlayAudio::with_volume) multiplier
/// times any randomization, or the silence a completed fade-out landed on)
/// multiplied by the fresh category level. Recomputing the product rather
/// than writing the category volume alone is what lets a settings change
/// reach every sound without flattening the mix into one level; a sound
/// spawned by hand without a [`BaseVolume`] reads as `1.0` and gets the bare
/// category volume, as before.
///
/// Samples mid [`FadeInAudio`]/[`FadeOutAudio`] are skipped — the fade owns
/// the node while present and would be clobbered by a bare volume write.
/// While [`apply_sound_damping`](crate::damping::apply_sound_damping) holds a
/// sound (any field alive, or the duck engaged), it runs later in the same
/// frame and extends the same product with its own two factors, so the write
/// here is folded into the damped value rather than fighting it.
pub fn update_category_volumes<C: AudioCategory>(
    config: Res<C::Config>,
    samples: Query<
        (&C, &SampleEffects, Option<&BaseVolume>),
        (Without<FadeInAudio>, Without<FadeOutAudio>),
    >,
    mut volume_nodes: Query<&mut VolumeNode>,
) {
    for (category, effects, base) in &samples {
        let target_volume = base.copied().unwrap_or_default().resolve(category.volume(&config));
        if let Ok(mut vol_node) = volume_nodes.get_effect_mut(effects) {
            vol_node.volume = Volume::Linear(target_volume);
        }
    }
}
