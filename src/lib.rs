//! # msg_seedling
//!
//! A seedling-powered audio management crate for Bevy games.
//!
//! Built on [`bevy_seedling`](https://crates.io/crates/bevy_seedling) (firewheel),
//! providing category-based volume control, spatial audio, and playback
//! randomization through a simple message-based API.
//!
//! ## Quick Start
//!
//! 1. Define your audio categories:
//!
//! ```rust,ignore
//! use bevy::prelude::*;
//! use msg_seedling::prelude::*;
//!
//! #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
//! #[reflect(Component)]
//! pub enum Sound {
//!     #[default]
//!     Music,
//!     Sfx,
//!     Ambience,
//! }
//!
//! #[derive(Resource, Clone, Default)]
//! pub struct GameAudioConfig {
//!     pub master: f32,
//!     pub music: f32,
//!     pub sfx: f32,
//!     pub ambience: f32,
//! }
//!
//! impl AudioConfig for GameAudioConfig {
//!     fn master_volume(&self) -> f32 { self.master }
//! }
//!
//! impl AudioCategory for Sound {
//!     type Config = GameAudioConfig;
//!     fn volume(&self, config: &GameAudioConfig) -> f32 {
//!         match self {
//!             Sound::Music => config.music,
//!             Sound::Sfx => config.sfx,
//!             Sound::Ambience => config.ambience,
//!         }
//!     }
//! }
//! ```
//!
//! 2. Add the plugin:
//!
//! ```rust,ignore
//! app.add_plugins(MsgSeedlingPlugin::<Sound>::default());
//! ```
//!
//! 3. Play audio:
//!
//! ```rust,ignore
//! fn play_sounds(mut writer: MessageWriter<PlayAudio<Sound>>) {
//!     // One-shot with default randomization
//!     writer.write(PlayAudio::new(sfx_handle, Sound::Sfx));
//!
//!     // Looping music
//!     writer.write(PlayAudio::new(music_handle, Sound::Music).looping());
//!
//!     // Spatial, attached to an entity
//!     writer.write(PlayAudio::new(step_handle, Sound::Sfx).with_parent(player_entity));
//!
//!     // Spatial at a world position
//!     writer.write(PlayAudio::new(explosion_handle, Sound::Sfx).at(Vec2::new(100.0, 50.0)));
//! }
//! ```

#[cfg(not(target_arch = "wasm32"))]
mod device_follow;
mod handlers;
mod messages;
mod randomization;
#[cfg(test)]
mod tests;
mod traits;
mod volume;

#[cfg(not(target_arch = "wasm32"))]
pub use device_follow::FollowDefaultAudioDevice;
pub use messages::{FadeAudio, PlayAudio, SpatialPosition, StopAudio};
pub use randomization::{DefaultRandomization, Randomization};
pub use traits::{AudioCategory, AudioConfig};

use bevy::prelude::*;
use bevy_seedling::prelude::*;

/// Main plugin for `msg_seedling`.
///
/// Registers messages and adds all handler and volume systems.
///
/// **Important:** You must also add `SeedlingPlugin::default()` (or your chosen backend)
/// before adding this plugin.
///
/// # Type Parameters
///
/// - `C`: Your audio category type implementing [`AudioCategory`].
///
/// # Configuration
///
/// ```rust,ignore
/// // Default: ±20% randomization for both volume and speed
/// app.add_plugins(MsgSeedlingPlugin::<Sound>::default());
///
/// // Custom randomization defaults and spatial scale
/// app.add_plugins(
///     MsgSeedlingPlugin::<Sound>::new()
///         .with_default_randomization(DefaultRandomization {
///             volume: Some(0.1),
///             speed: None,
///         })
///         .with_spatial_scale(Vec3::splat(1.0 / 100.0))
/// );
/// ```
pub struct MsgSeedlingPlugin<C: AudioCategory> {
    default_randomization: DefaultRandomization,
    spatial_scale: Option<Vec3>,
    /// `Some` = follow the OS default output device with this configuration.
    /// Ignored on wasm, where the browser owns device routing.
    #[cfg(not(target_arch = "wasm32"))]
    device_follow: Option<FollowDefaultAudioDevice>,
    _phantom: std::marker::PhantomData<C>,
}

impl<C: AudioCategory> Default for MsgSeedlingPlugin<C> {
    fn default() -> Self {
        Self {
            default_randomization: DefaultRandomization::default(),
            spatial_scale: None,
            #[cfg(not(target_arch = "wasm32"))]
            device_follow: Some(FollowDefaultAudioDevice::default()),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<C: AudioCategory> MsgSeedlingPlugin<C> {
    /// Creates a new plugin with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the default randomization for all audio.
    #[must_use]
    pub fn with_default_randomization(mut self, defaults: DefaultRandomization) -> Self {
        self.default_randomization = defaults;
        self
    }

    /// Sets the spatial audio scale.
    ///
    /// For 2D games, use `Vec3::splat(1.0 / pixels_per_unit)`.
    /// For example, `Vec3::splat(1.0 / 100.0)` means 100 pixels = 1 audio unit.
    #[must_use]
    pub fn with_spatial_scale(mut self, scale: Vec3) -> Self {
        self.spatial_scale = Some(scale);
        self
    }

    /// Configures how the OS default output device is followed.
    ///
    /// Following is enabled by default with a one-second poll interval; use
    /// this to change the interval. No effect on wasm.
    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_device_follow(mut self, settings: FollowDefaultAudioDevice) -> Self {
        self.device_follow = Some(settings);
        self
    }

    /// Disables following the OS default output device.
    ///
    /// The stream then stays on the device it was opened with until it fails
    /// (`bevy_seedling` still recovers from outright device loss). No effect
    /// on wasm.
    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub fn without_device_follow(mut self) -> Self {
        self.device_follow = None;
        self
    }
}

impl<C: AudioCategory> Plugin for MsgSeedlingPlugin<C> {
    fn build(&self, app: &mut App) {
        // Insert resources
        app.insert_resource(self.default_randomization.clone());

        if let Some(scale) = self.spatial_scale {
            app.insert_resource(DefaultSpatialScale(scale));
        }

        // Register messages
        app.add_message::<PlayAudio<C>>();
        app.add_message::<StopAudio<C>>();
        app.add_message::<FadeAudio<C>>();

        // Add systems
        app.add_systems(
            Update,
            (
                handlers::handle_play_audio::<C>,
                handlers::handle_stop_audio::<C>,
                handlers::handle_fade_audio::<C>,
                volume::update_master_volume::<C>.run_if(resource_changed::<C::Config>),
                volume::update_category_volumes::<C>.run_if(resource_changed::<C::Config>),
            ),
        );

        // Follow the OS default output device (native only)
        #[cfg(not(target_arch = "wasm32"))]
        {
            use bevy_seedling::context::{AudioContext, AudioStreamConfig};

            app.register_type::<FollowDefaultAudioDevice>();
            app.init_resource::<device_follow::FollowDefaultState>();
            if let Some(settings) = self.device_follow.clone() {
                app.insert_resource(settings);
            }
            app.add_systems(
                Update,
                device_follow::follow_default_device.run_if(
                    resource_exists::<FollowDefaultAudioDevice>
                        .and(resource_exists::<AudioContext>)
                        .and(resource_exists::<AudioStreamConfig>),
                ),
            );
            app.add_observer(device_follow::resync_on_stream_start);
            app.add_observer(device_follow::resync_on_stream_restart);
        }
    }
}

/// Re-export of handler systems for custom scheduling.
pub mod audio_systems {
    pub use crate::handlers::{handle_fade_audio, handle_play_audio, handle_stop_audio};
    pub use crate::volume::{update_category_volumes, update_master_volume};
}

/// Prelude module for convenient imports.
pub mod prelude {
    pub use crate::MsgSeedlingPlugin;
    #[cfg(not(target_arch = "wasm32"))]
    pub use crate::device_follow::FollowDefaultAudioDevice;
    pub use crate::messages::{FadeAudio, PlayAudio, SpatialPosition, StopAudio};
    pub use crate::randomization::{DefaultRandomization, Randomization};
    pub use crate::traits::{AudioCategory, AudioConfig};

    // Re-export commonly needed seedling types
    pub use bevy_seedling::prelude::{
        DefaultSpatialScale, MainBus, SpatialListener2D, SpatialListener3D, VolumeNode,
    };
    pub use bevy_seedling::sample::AudioSample;
}
