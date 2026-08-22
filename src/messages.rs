use bevy::prelude::*;

use crate::randomization::Randomization;
use crate::traits::AudioCategory;

/// Spatial positioning for audio.
#[derive(Clone, Copy, Debug, Reflect)]
pub enum SpatialPosition {
    Vec2(Vec2),
    Vec3(Vec3),
}

impl From<Vec2> for SpatialPosition {
    fn from(v: Vec2) -> Self {
        Self::Vec2(v)
    }
}

impl From<Vec3> for SpatialPosition {
    fn from(v: Vec3) -> Self {
        Self::Vec3(v)
    }
}

impl SpatialPosition {
    /// Converts to a `Vec3`, extending `Vec2` with `z = 0.0`.
    pub fn as_vec3(self) -> Vec3 {
        match self {
            Self::Vec2(v) => v.extend(0.0),
            Self::Vec3(v) => v,
        }
    }
}

/// Message to play an audio sample.
///
/// Sent via `MessageWriter<PlayAudio<C>>`. The handler spawns a seedling
/// `SamplePlayer` routed through the appropriate pool with volume and
/// spatial settings applied.
///
/// # Examples
///
/// ```
/// # use bevy::prelude::*;
/// # use msg_seedling::prelude::*;
/// # #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
/// # #[reflect(Component)]
/// # enum Sound { #[default] Sfx, Music }
/// # #[derive(Resource, Clone, Default)]
/// # struct Settings;
/// # impl AudioConfig for Settings { fn master_volume(&self) -> f32 { 1.0 } }
/// # impl AudioCategory for Sound {
/// #     type Config = Settings;
/// #     fn volume(&self, _config: &Settings) -> f32 { 1.0 }
/// # }
/// fn play_sounds(mut writer: MessageWriter<PlayAudio<Sound>>) {
///     # let handle: Handle<AudioSample> = Handle::default();
///     # let entity = Entity::from_bits(1);
///     // One-shot with plugin default randomization
///     writer.write(PlayAudio::new(handle.clone(), Sound::Sfx));
///
///     // Looping music, exact playback
///     writer.write(
///         PlayAudio::new(handle.clone(), Sound::Music)
///             .looping()
///             .randomized(Randomization::VolumeAndSpeed { volume: 0.0, speed: 0.0 })
///     );
///
///     // Spatial, attached to entity
///     writer.write(PlayAudio::new(handle.clone(), Sound::Sfx).with_parent(entity));
///
///     // Spatial at world position
///     writer.write(PlayAudio::new(handle, Sound::Sfx).at(Vec2::new(100.0, 50.0)));
/// }
/// # let mut app = App::new();
/// # app.add_plugins(MinimalPlugins);
/// # app.add_message::<PlayAudio<Sound>>();
/// # app.add_systems(Update, play_sounds);
/// # app.update();
/// ```
#[derive(Message, Clone)]
pub struct PlayAudio<C: AudioCategory> {
    /// Handle to the audio sample.
    pub handle: Handle<bevy_seedling::sample::AudioSample>,
    /// The audio category for volume control.
    pub category: C,
    /// Whether to loop endlessly. Default: `false` (play once, despawn).
    pub looping: bool,
    /// Parent entity — implies spatial audio via `ChildOf`.
    pub parent: Option<Entity>,
    /// Spatial position in world space.
    pub position: Option<SpatialPosition>,
    /// Base volume multiplier (before category/master). Default: `1.0`.
    pub volume: f32,
    /// Randomization settings. Default: [`Randomization::Default`].
    pub randomization: Randomization,
}

impl<C: AudioCategory> PlayAudio<C> {
    /// Creates a new play audio message with default settings.
    ///
    /// Plays once, despawns on completion, uses plugin default randomization.
    #[must_use]
    pub fn new(handle: Handle<bevy_seedling::sample::AudioSample>, category: C) -> Self {
        Self {
            handle,
            category,
            looping: false,
            parent: None,
            position: None,
            volume: 1.0,
            randomization: Randomization::Default,
        }
    }

    /// Sets playback to loop endlessly.
    #[must_use]
    pub fn looping(mut self) -> Self {
        self.looping = true;
        self
    }

    /// Attaches to a parent entity via `ChildOf`. Implies spatial audio.
    #[must_use]
    pub fn with_parent(mut self, parent: Entity) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Sets the spatial position.
    #[must_use]
    pub fn at(mut self, position: impl Into<SpatialPosition>) -> Self {
        self.position = Some(position.into());
        self
    }

    /// Sets the base volume multiplier (before category and master).
    #[must_use]
    pub fn with_volume(mut self, volume: f32) -> Self {
        self.volume = volume;
        self
    }

    /// Sets the randomization mode.
    #[must_use]
    pub fn randomized(mut self, randomization: Randomization) -> Self {
        self.randomization = randomization;
        self
    }
}

/// Message to stop audio by category.
///
/// # Examples
///
/// ```
/// # use bevy::prelude::*;
/// # use msg_seedling::prelude::*;
/// # #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
/// # #[reflect(Component)]
/// # enum Sound { #[default] Sfx, Music }
/// # #[derive(Resource, Clone, Default)]
/// # struct Settings;
/// # impl AudioConfig for Settings { fn master_volume(&self) -> f32 { 1.0 } }
/// # impl AudioCategory for Sound {
/// #     type Config = Settings;
/// #     fn volume(&self, _config: &Settings) -> f32 { 1.0 }
/// # }
/// fn stop_sounds(mut writer: MessageWriter<StopAudio<Sound>>) {
///     // Stop all music
///     writer.write(StopAudio::category(Sound::Music));
///
///     // Stop everything
///     writer.write(StopAudio::<Sound>::all());
/// }
/// # let mut app = App::new();
/// # app.add_plugins(MinimalPlugins);
/// # app.add_message::<StopAudio<Sound>>();
/// # app.add_systems(Update, stop_sounds);
/// # app.update();
/// ```
#[derive(Message, Clone)]
pub struct StopAudio<C: AudioCategory> {
    /// The category to stop. `None` = stop all audio.
    pub category: Option<C>,
}

impl<C: AudioCategory> StopAudio<C> {
    /// Stop all audio matching a specific category.
    #[must_use]
    pub fn category(category: C) -> Self {
        Self {
            category: Some(category),
        }
    }

    /// Stop all audio regardless of category.
    #[must_use]
    pub fn all() -> Self {
        Self { category: None }
    }
}

/// Message to fade out audio by category.
///
/// Uses seedling's `VolumeFade` for smooth audio-thread fading.
///
/// # Examples
///
/// ```
/// # use bevy::prelude::*;
/// # use msg_seedling::prelude::*;
/// # #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
/// # #[reflect(Component)]
/// # enum Sound { #[default] Sfx, Music }
/// # #[derive(Resource, Clone, Default)]
/// # struct Settings;
/// # impl AudioConfig for Settings { fn master_volume(&self) -> f32 { 1.0 } }
/// # impl AudioCategory for Sound {
/// #     type Config = Settings;
/// #     fn volume(&self, _config: &Settings) -> f32 { 1.0 }
/// # }
/// fn fade_music(mut writer: MessageWriter<FadeAudio<Sound>>) {
///     writer.write(FadeAudio::new(Sound::Music, 2.0));
/// }
/// # let mut app = App::new();
/// # app.add_plugins(MinimalPlugins);
/// # app.add_message::<FadeAudio<Sound>>();
/// # app.add_systems(Update, fade_music);
/// # app.update();
/// ```
#[derive(Message, Clone)]
pub struct FadeAudio<C: AudioCategory> {
    /// The category to fade out.
    pub category: C,
    /// Fade duration in seconds.
    pub duration_secs: f32,
}

impl<C: AudioCategory> FadeAudio<C> {
    /// Creates a new fade-out message.
    #[must_use]
    pub fn new(category: C, duration_secs: f32) -> Self {
        Self {
            category,
            duration_secs,
        }
    }
}
