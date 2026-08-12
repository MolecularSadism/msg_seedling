use bevy::prelude::*;

/// Base trait for audio categories that provide volume multipliers.
///
/// Games implement this for their own audio category enum. Each variant
/// maps to a volume level from the configuration resource.
///
/// # Example
///
/// ```rust,ignore
/// #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
/// #[reflect(Component)]
/// pub enum Sound {
///     #[default]
///     Music,
///     Sfx,
///     Ambience,
///     UI,
/// }
///
/// impl AudioCategory for Sound {
///     type Config = GameAudioConfig;
///     fn volume(&self, config: &GameAudioConfig) -> f32 {
///         match self {
///             Sound::Music => config.music,
///             Sound::Sfx => config.sfx,
///             Sound::Ambience => config.ambience,
///             Sound::UI => config.ui,
///         }
///     }
/// }
/// ```
pub trait AudioCategory:
    Component
    + Clone
    + Copy
    + Default
    + std::fmt::Debug
    + PartialEq
    + Eq
    + std::hash::Hash
    + Send
    + Sync
    + 'static
{
    /// The configuration resource that provides volume settings.
    type Config: AudioConfig;

    /// Returns the volume multiplier for this category (0.0–1.0).
    fn volume(&self, config: &Self::Config) -> f32;
}

/// Trait for audio configuration resources.
///
/// Provides master volume and an optional mute toggle.
///
/// # Example
///
/// ```rust,ignore
/// #[derive(Resource, Clone, Default, Reflect)]
/// #[reflect(Resource)]
/// pub struct GameAudioConfig {
///     pub master: f32,
///     pub music: f32,
///     pub sfx: f32,
///     pub muted: bool,
/// }
///
/// impl AudioConfig for GameAudioConfig {
///     fn master_volume(&self) -> f32 { self.master }
///     fn is_muted(&self) -> bool { self.muted }
/// }
/// ```
pub trait AudioConfig: Resource + Clone + Default + Send + Sync + 'static {
    /// Returns the master volume level (0.0–1.0).
    fn master_volume(&self) -> f32;

    /// Returns whether audio is globally muted. Default: `false`.
    fn is_muted(&self) -> bool {
        false
    }

    /// Returns 0.0 if muted, otherwise `master_volume()`.
    fn effective_volume(&self) -> f32 {
        if self.is_muted() {
            0.0
        } else {
            self.master_volume()
        }
    }
}
