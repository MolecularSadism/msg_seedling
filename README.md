# msg_seedling

A [bevy_seedling](https://crates.io/crates/bevy_seedling)-powered audio management crate for [Bevy](https://bevyengine.org/) games.

Built on the [Firewheel](https://github.com/BillyDM/Firewheel) audio engine via `bevy_seedling`, providing:

- **Category-based volume** -- define your own audio categories for grouped volume control
- **Message-based API** -- fire-and-forget `PlayAudio`, `StopAudio`, `FadeAudio` via Bevy messages
- **Spatial audio** -- 2D/3D positioning with `Option<Vec2>` / `Option<Vec3>`, or parent entity attachment
- **Randomization** -- configurable per-play volume and speed deviation with plugin-wide defaults
- **Smooth fading** -- audio-thread `VolumeFade` for glitch-free fade-outs

## Quick Start

### 1. Dependencies

Add to your `Cargo.toml`. Note: you must disable Bevy's default `bevy_audio` feature.

```toml
[dependencies]
msg_seedling = "0.1"
bevy_seedling = "0.7"
bevy = { version = "0.18", default-features = false, features = [
    "2d_bevy_render", "default_app", "picking", "scene",
    "ui_api", "ui_bevy_render",
    "android-game-activity", "bevy_gilrs", "bevy_winit",
    "default_font", "multi_threaded", "std", "sysinfo_plugin",
    "wayland", "webgl2", "x11",
] }
```

### 2. Define categories

No predefined Music/Sfx split -- you define what makes sense for your game:

```rust
use bevy::prelude::*;
use msg_seedling::prelude::*;

#[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
#[reflect(Component)]
pub enum Sound {
    #[default]
    Music,
    Sfx,
    Ambience,
    UI,
}

#[derive(Resource, Clone, Default)]
pub struct AudioSettings {
    pub master: f32,
    pub music: f32,
    pub sfx: f32,
    pub ambience: f32,
    pub ui: f32,
    pub muted: bool,
}

impl AudioConfig for AudioSettings {
    fn master_volume(&self) -> f32 { self.master }
    fn is_muted(&self) -> bool { self.muted }
}

impl AudioCategory for Sound {
    type Config = AudioSettings;
    fn volume(&self, config: &AudioSettings) -> f32 {
        match self {
            Sound::Music => config.music,
            Sound::Sfx => config.sfx,
            Sound::Ambience => config.ambience,
            Sound::UI => config.ui,
        }
    }
}
```

### 3. Add plugins

```rust
fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(SeedlingPlugin::default())
        .add_plugins(MsgSeedlingPlugin::<Sound>::default())
        .init_resource::<AudioSettings>()
        .run();
}
```

### 4. Play audio

```rust
fn play_sounds(
    mut writer: MessageWriter<PlayAudio<Sound>>,
    server: Res<AssetServer>,
) {
    // One-shot SFX with default randomization (+-20% volume and speed)
    writer.write(PlayAudio::new(server.load("hit.ogg"), Sound::Sfx));

    // Looping music, no randomization
    writer.write(
        PlayAudio::new(server.load("theme.ogg"), Sound::Music)
            .looping()
            .randomized(Randomization::VolumeAndSpeed { volume: 0.0, speed: 0.0 })
    );

    // Spatial SFX attached to an entity (follows its Transform)
    writer.write(
        PlayAudio::new(server.load("footstep.ogg"), Sound::Sfx)
            .with_parent(player_entity)
    );

    // Spatial SFX at a world position
    writer.write(
        PlayAudio::new(server.load("explosion.ogg"), Sound::Sfx)
            .at(Vec2::new(500.0, 300.0))
    );
}
```

### 5. Stop and fade

```rust
fn transition_music(
    mut stop: MessageWriter<StopAudio<Sound>>,
    mut fade: MessageWriter<FadeAudio<Sound>>,
) {
    // Fade out current music over 2 seconds
    fade.write(FadeAudio::new(Sound::Music, 2.0));

    // Immediately stop all SFX
    stop.write(StopAudio::category(Sound::Sfx));

    // Stop everything
    stop.write(StopAudio::<Sound>::all());
}
```

## Randomization

Every `PlayAudio` message carries a `Randomization` setting:

| Variant | Volume | Speed |
|---------|--------|-------|
| `Default` | Plugin default | Plugin default |
| `Volume(0.3)` | +-30% | Plugin default |
| `Speed(0.1)` | Plugin default | +-10% |
| `VolumeAndSpeed { volume: 0.2, speed: 0.1 }` | +-20% | +-10% |

Plugin defaults are configured at init (default: `Some(0.2)` for both):

```rust
MsgSeedlingPlugin::<Sound>::new()
    .with_default_randomization(DefaultRandomization {
        volume: Some(0.15),  // +-15%
        speed: None,         // no speed randomization
    })
```

Volume randomization uses a system-local `SmallRng`. Speed randomization delegates to seedling's built-in `RandomPitch`.

## Spatial Audio

Spatial audio uses seedling's `SpatialPool` with `SpatialBasicNode` (stereo panning + distance attenuation).

You must add a `SpatialListener2D` (or `SpatialListener3D`) to your listener entity:

```rust
// On your camera or player
commands.spawn((Camera2d::default(), SpatialListener2D));
```

For 2D games, configure the spatial scale so pixel distances map to audio units:

```rust
MsgSeedlingPlugin::<Sound>::new()
    .with_spatial_scale(Vec3::splat(1.0 / 100.0))  // 100 pixels = 1 audio unit
```

## Following the OS Default Output Device

`cpal` binds the audio stream to the concrete device that was the default when
the stream opened and never rebinds it, and `bevy_seedling` only restarts the
stream when it errors out (device removed). So switching the system default
output — without the old device disappearing — would leave audio playing on
the old device.

On native targets, `device_follow::plugin` polls the OS default output device
(every second by default) and asks `bevy_seedling` to restart the stream when
it changes, as long as `AudioStreamConfig` does not pin a specific output
device. A pinned device is always respected, and costs no device enumeration.

This is a standalone, optional plugin: it has nothing to do with audio
categories, so it is not part of `MsgSeedlingPlugin<C>` and is added once
regardless of how many category types the app uses. On wasm the browser owns
device routing, so the module is not compiled there.

```rust
app.add_plugins(msg_seedling::device_follow::plugin);
```

Insert or remove the `FollowDefaultAudioDevice` resource to toggle the
behavior at runtime, or set its `poll_interval` to retime the poll:

```rust
commands.insert_resource(FollowDefaultAudioDevice {
    poll_interval: Duration::from_millis(500),
});
```

## Architecture

`msg_seedling` is a thin convenience layer over `bevy_seedling`. It does not abstract away the node graph -- power users can use seedling's `Connect`, `SamplerPool`, effects chains, and bus routing directly alongside `msg_seedling`.

### Volume model

| Layer | Location | Updated |
|-------|----------|---------|
| Master | `MainBus` `VolumeNode` | On config change |
| Category | Per-sample `VolumeNode` effect | On config change |
| Per-play | Baked into `VolumeNode` at spawn | Spawn only |

### Voice management

Handled entirely by seedling's pool system (`DefaultPool` for non-spatial, `SpatialPool` for spatial). No manual concurrency limits needed.

## Bevy Compatibility

| msg_seedling | bevy_seedling | Bevy |
|-------------|---------------|------|
| 0.1         | 0.7           | 0.18 |

## License

MIT OR Apache-2.0
