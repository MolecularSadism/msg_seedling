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
//! ```
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
//! ```
//! # use bevy::prelude::*;
//! # use msg_seedling::prelude::*;
//! # #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
//! # #[reflect(Component)]
//! # enum Sound { #[default] Sfx }
//! # #[derive(Resource, Clone, Default)]
//! # struct GameAudioConfig;
//! # impl AudioConfig for GameAudioConfig {
//! #     fn master_volume(&self) -> f32 { 1.0 }
//! # }
//! # impl AudioCategory for Sound {
//! #     type Config = GameAudioConfig;
//! #     fn volume(&self, _config: &GameAudioConfig) -> f32 { 1.0 }
//! # }
//! # let mut app = App::new();
//! # app.add_plugins(MinimalPlugins);
//! # app.init_resource::<GameAudioConfig>();
//! app.add_plugins(MsgSeedlingPlugin::<Sound>::default());
//! ```
//!
//!    On native targets, [`device_follow::plugin`] is a separate, optional
//!    plugin that keeps the stream on the OS default output device as the
//!    user switches it. It is independent of the category type, so add it
//!    once:
//!
//! ```
//! # use bevy::prelude::*;
//! # let mut app = App::new();
//! # app.add_plugins(MinimalPlugins);
//! app.add_plugins(msg_seedling::device_follow::plugin);
//! # app.update();
//! ```
//!
//! 3. Play audio:
//!
//! ```
//! # use bevy::prelude::*;
//! # use msg_seedling::prelude::*;
//! # #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
//! # #[reflect(Component)]
//! # enum Sound { #[default] Sfx, Music }
//! # #[derive(Resource, Clone, Default)]
//! # struct GameAudioConfig;
//! # impl AudioConfig for GameAudioConfig {
//! #     fn master_volume(&self) -> f32 { 1.0 }
//! # }
//! # impl AudioCategory for Sound {
//! #     type Config = GameAudioConfig;
//! #     fn volume(&self, _config: &GameAudioConfig) -> f32 { 1.0 }
//! # }
//! fn play_sounds(mut writer: MessageWriter<PlayAudio<Sound>>) {
//!     # let sfx_handle: Handle<AudioSample> = Handle::default();
//!     # let music_handle = sfx_handle.clone();
//!     # let step_handle = sfx_handle.clone();
//!     # let explosion_handle = sfx_handle.clone();
//!     # let player_entity = Entity::from_bits(1);
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
//! # let mut app = App::new();
//! # app.add_plugins(MinimalPlugins);
//! # app.add_message::<PlayAudio<Sound>>();
//! # app.add_systems(Update, play_sounds);
//! # app.update();
//! ```

#[cfg(not(target_arch = "wasm32"))]
pub mod device_follow;
pub mod fade;
mod handlers;
mod messages;
mod randomization;
#[cfg(test)]
mod tests;
mod traits;
pub mod virtual_queue;
mod volume;

#[cfg(not(target_arch = "wasm32"))]
pub use device_follow::FollowDefaultAudioDevice;
pub use fade::{FadeInAudio, FadeOutAudio};
pub use messages::{FadeAudio, PlayAudio, SpatialPosition, StopAudio};
pub use randomization::{DefaultRandomization, Randomization};
pub use traits::{AudioCategory, AudioConfig};
pub use virtual_queue::{
    Audible, PlayQueuedAudio, Retiring, SignificanceEntry, StopQueuedAudio, VirtualSound,
    VirtualVoiceBudget, VirtualVoiceQueuePlugin, VoiceDecision, rank_by_significance,
};

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
/// ```
/// # use bevy::prelude::*;
/// # use msg_seedling::prelude::*;
/// # #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
/// # #[reflect(Component)]
/// # enum Sound { #[default] Sfx }
/// # #[derive(Resource, Clone, Default)]
/// # struct GameAudioConfig;
/// # impl AudioConfig for GameAudioConfig {
/// #     fn master_volume(&self) -> f32 { 1.0 }
/// # }
/// # impl AudioCategory for Sound {
/// #     type Config = GameAudioConfig;
/// #     fn volume(&self, _config: &GameAudioConfig) -> f32 { 1.0 }
/// # }
/// # let mut app = App::new();
/// # app.add_plugins(MinimalPlugins);
/// # app.init_resource::<GameAudioConfig>();
/// // Default: ±20% randomization for both volume and speed
/// app.add_plugins(MsgSeedlingPlugin::<Sound>::default());
///
/// # let mut app = App::new();
/// # app.add_plugins(MinimalPlugins);
/// # app.init_resource::<GameAudioConfig>();
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
    _phantom: std::marker::PhantomData<C>,
}

impl<C: AudioCategory> Default for MsgSeedlingPlugin<C> {
    fn default() -> Self {
        Self {
            default_randomization: DefaultRandomization::default(),
            spatial_scale: None,
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
    pub use crate::fade::{FadeInAudio, FadeOutAudio};
    pub use crate::messages::{FadeAudio, PlayAudio, SpatialPosition, StopAudio};
    pub use crate::randomization::{DefaultRandomization, Randomization};
    pub use crate::traits::{AudioCategory, AudioConfig};
    pub use crate::virtual_queue::{
        Audible, PlayQueuedAudio, Retiring, StopQueuedAudio, VirtualSound, VirtualVoiceBudget,
        VirtualVoiceQueuePlugin,
    };

    // Re-export commonly needed seedling types
    pub use bevy_seedling::prelude::{
        DefaultSpatialScale, MainBus, SpatialListener2D, SpatialListener3D, VolumeNode,
    };
    pub use bevy_seedling::sample::AudioSample;
}
