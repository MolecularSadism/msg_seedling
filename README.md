# msg_seedling

A [bevy_seedling](https://crates.io/crates/bevy_seedling)-powered audio management crate for [Bevy](https://bevyengine.org/) games.

Built on the [Firewheel](https://github.com/BillyDM/Firewheel) audio engine via `bevy_seedling`, providing:

- **Category-based volume** -- define your own audio categories for grouped volume control
- **Message-based API** -- fire-and-forget `PlayAudio`, `StopAudio`, `FadeAudio` via Bevy messages
- **Spatial audio** -- 2D/3D positioning with `Option<Vec2>` / `Option<Vec3>`, or parent entity attachment
- **Randomization** -- configurable per-play volume and speed deviation with plugin-wide defaults
- **Smooth fading** -- audio-thread `VolumeFade` for glitch-free fade-outs, plus reusable `FadeInAudio`/`FadeOutAudio` components for any sample entity
- **Virtual voice queue** -- opt-in, significance-ranked voice budget that crossfades between sounds instead of hard-cutting when a pool runs out of room

## Quick Start

### 1. Dependencies

Add to your `Cargo.toml`. Note: you must disable Bevy's default `bevy_audio` feature.

```toml
[dependencies]
msg_seedling = "0.2"
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

## Virtual Voice Queue

`PlayAudio` hands a request straight to seedling's pool system. If the pool is
already at its configured size, seedling steals whichever voice loses its
internal priority tie-break -- instantly, with no fade. Two sounds trading
places this way is audible as a hard cut, not a transition.

`VirtualVoiceQueuePlugin<C>` is an opt-in alternative for category `C`: every
`PlayQueuedAudio` request becomes a *virtual* entry ranked by **audible
significance** (its resolved volume, times an optional priority weight), and
only the top `VirtualVoiceBudget::max_audible` entries ever get a real
`SamplePlayer`. When ranking changes, entries cross the line via
`FadeInAudio`/`FadeOutAudio` instead of a hard cut -- promoting one voice
while demoting another reads as an actual crossfade. Entries that don't make
the cut aren't rejected outright: they wait as silent virtual entries and get
promoted the moment a slot frees up or they become the most significant thing
requested, up to `VirtualVoiceBudget::max_wait` -- past that they're dropped.

```rust
app.add_plugins(
    VirtualVoiceQueuePlugin::<Sound>::new()
        .with_budget(
            VirtualVoiceBudget::<Sound>::new(16)
                .with_crossfade(Duration::from_millis(50))
                .with_max_wait(Duration::from_millis(500)),
        ),
);

fn play_sounds(mut writer: MessageWriter<PlayQueuedAudio<Sound>>, server: Res<AssetServer>) {
    // A quiet, low-priority ambience request: waits as virtual if the queue
    // is full of louder things, promoted if room frees up.
    writer.write(PlayQueuedAudio::new(server.load("wind.ogg"), Sound::Ambience).with_volume(0.2));

    // A loud explosion: outranks quieter voices and crossfades in, smoothly
    // displacing whichever currently-audible voice ranks lowest.
    writer.write(PlayQueuedAudio::new(server.load("explosion.ogg"), Sound::Sfx).with_priority(2.0));
}
```

This queue is independent of `PlayAudio`/`StopAudio`/`FadeAudio` and the
per-category volume-update systems -- a promoted entry does not carry the
bare `C` component those systems match on, so mixing the two paths for the
same category is deliberately not wired together in this version. Budgets
are scoped per category type `C`, so e.g. music and SFX never compete for
the same slots. Significance does not currently factor in distance for
spatial sounds -- bake any distance attenuation into `.with_volume()` or
`.with_priority()` before sending.

### Bring your own pool

`VirtualVoiceQueuePlugin` always promotes into seedling's built-in
`SpatialPool`/`DefaultPool`. If your game needs a different pool -- one with
a custom effects chain (a low-pass filter for a muffling system, say) -- the
ranking decision itself is exposed as a pure function so you can drive your
own promotion/demotion and still get the crossfade for free:

```rust
use msg_seedling::{FadeInAudio, FadeOutAudio, SignificanceEntry, VoiceDecision, rank_by_significance};

let decisions = rank_by_significance(&entries, max_audible, retiring_count);
// For each `VoiceDecision::Promote`: insert your own pool marker + effects
// chain + `FadeInAudio::new(crossfade, target_volume)`.
// For each `VoiceDecision::Demote`: insert `FadeOutAudio::new(crossfade)`.
```

`FadeInAudio`/`FadeOutAudio` only need `SampleEffects` to work, so they're
usable with any pool, not just the ones this crate spawns.

## Architecture

`msg_seedling` is a thin convenience layer over `bevy_seedling`. It does not abstract away the node graph -- power users can use seedling's `Connect`, `SamplerPool`, effects chains, and bus routing directly alongside `msg_seedling`.

### Volume model

| Layer | Location | Updated |
|-------|----------|---------|
| Master | `MainBus` `VolumeNode` | On config change |
| Category | Per-sample `VolumeNode` effect | On config change |
| Per-play | Baked into `VolumeNode` at spawn | Spawn only |

### Voice management

`PlayAudio` is handled entirely by seedling's pool system (`DefaultPool` for
non-spatial, `SpatialPool` for spatial) -- no manual concurrency limits, and
no protection against a hard-cut steal when the pool is full. For a voice
budget that crossfades instead of cutting, use `VirtualVoiceQueuePlugin<C>`
and `PlayQueuedAudio` (see "Virtual Voice Queue" above).

## Bevy Compatibility

| msg_seedling | bevy_seedling | Bevy |
|-------------|---------------|------|
| 0.2         | 0.7           | 0.18 |
| 0.1         | 0.7           | 0.18 |

## License

MIT OR Apache-2.0
