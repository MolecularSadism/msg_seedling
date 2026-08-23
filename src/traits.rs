use bevy::prelude::*;

/// Base trait for audio categories that provide volume multipliers.
///
/// Games implement this for their own audio category enum. Each variant
/// maps to a volume level from the configuration resource. Categories must
/// derive `Reflect` so components generic over them (e.g.
/// [`VirtualSound`](crate::virtual_queue::VirtualSound)) can be registered
/// for reflection.
///
/// # Example
///
/// ```
/// # use bevy::prelude::*;
/// # use msg_seedling::prelude::*;
/// # #[derive(Resource, Clone, Default)]
/// # pub struct GameAudioConfig {
/// #     pub music: f32,
/// #     pub sfx: f32,
/// #     pub ambience: f32,
/// #     pub ui: f32,
/// # }
/// # impl AudioConfig for GameAudioConfig {
/// #     fn master_volume(&self) -> f32 { 1.0 }
/// # }
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
/// #
/// # let config = GameAudioConfig { music: 0.5, ..Default::default() };
/// # assert_eq!(Sound::Music.volume(&config), 0.5);
/// # assert_eq!(Sound::Sfx.volume(&config), 0.0);
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
    + bevy::reflect::Reflectable
    + FromReflect
    + Send
    + Sync
    + 'static
{
    /// The configuration resource that provides volume settings.
    type Config: AudioConfig;

    /// Returns the volume multiplier for this category (0.0–1.0).
    fn volume(&self, config: &Self::Config) -> f32;

    /// Whether sounds of this category may be damped by a
    /// [`SoundDampingField`](crate::damping::SoundDampingField) at all.
    ///
    /// Interface audio is the classic exemption: a menu click is not an
    /// event in the world, so a field in the world does not get to muffle
    /// it. Defaults to `true`.
    fn is_dampable(&self) -> bool {
        true
    }
}

/// Trait for audio configuration resources.
///
/// Provides master volume and an optional mute toggle.
///
/// # Example
///
/// ```
/// # use bevy::prelude::*;
/// # use msg_seedling::prelude::*;
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
/// #
/// # let config = GameAudioConfig { master: 0.8, muted: true, ..Default::default() };
/// # assert_eq!(config.master_volume(), 0.8);
/// # assert_eq!(config.effective_volume(), 0.0);
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
