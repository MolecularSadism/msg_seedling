# msg_seedling

A [bevy_seedling](https://crates.io/crates/bevy_seedling)-powered audio management crate for [Bevy](https://bevyengine.org/) games.

Built on the [Firewheel](https://github.com/BillyDM/Firewheel) audio engine via `bevy_seedling`, providing:

- **Category-based volume** -- define your own audio categories for grouped volume control
- **Message-based API** -- fire-and-forget `PlayAudio`, `StopAudio`, `FadeAudio` via Bevy messages
- **Spatial audio** -- 2D/3D positioning with `Option<Vec2>` / `Option<Vec3>`, or parent entity attachment
- **Randomization** -- configurable per-play volume and speed deviation with plugin-wide defaults
- **Smooth fading** -- audio-thread ramps for glitch-free fade-outs, reusable `FadeInAudio`/`FadeOutAudio` components for any sample entity, and `FadeMix` for taking the whole bus down at once
- **Sound damping fields** -- world-space volumes that muffle what crosses them, on volume, low-pass cutoff and pitch at once (authorable in data behind the `serde` feature)
- **Sidechain ducking** -- one mix-wide envelope that steps the routine mix back so a crucial cue lands
- **Virtual voice queue** -- opt-in voice allocation ranked by how loud each sound actually reaches the listener (distance and damping included), with per-pool and per-sample budgets and hard priority tiers; it crossfades instead of hard-cutting, displaced loops wait silently and come back, and admission controls drop the one-shots not worth queueing

## Quick Start

### 1. Dependencies

Add to your `Cargo.toml`. Disable Bevy's `bevy_audio` feature: it rides along with
the `2d`, `3d` and `ui` feature groups, and nothing here plays through it.

```toml
[dependencies]
msg_seedling = "0.6"
bevy_seedling = "0.7"
bevy = { version = "0.18", default-features = false, features = [
    "2d_bevy_render", "default_app", "picking", "scene",
    "ui_api", "ui_bevy_render",
    "android-game-activity", "bevy_gilrs", "bevy_winit",
    "default_font", "multi_threaded", "std", "sysinfo_plugin",
    "wayland", "webgl2", "x11",
] }
```

Leaving `bevy_audio` on is supported but pointless. Every sound is a
`bevy_seedling` sample, so Bevy's audio stack is overridden either way, while
still opening a second OS output stream, registering competing loaders for
`ogg`/`wav`/`mp3`, and — on Linux — pulling in an `alsa-sys` major that cannot
link alongside firewheel's. `MsgSeedlingPlugin` warns at startup whenever it
finds Bevy's `AudioPlugin` live; if you cannot drop the feature, at least build
`DefaultPlugins` with `.disable::<bevy::audio::AudioPlugin>()`.

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

`MsgSeedlingPlugin<C>` is the only one you need. Everything else is opt-in and
composes with it -- add what your game actually uses:

| Plugin | Scoped to | Gives you |
|--------|-----------|-----------|
| `DampingPlugin::<C>` | one category type | [sound damping fields](#sound-damping-fields), and the [ducking envelope](#sidechain-ducking) they share a volume write with |
| `VirtualVoiceQueuePlugin::<C>` | one category type | the [significance-ranked voice budget](#virtual-voice-queue) |
| `MixFadePlugin::<Config>` | one config type | [`FadeMix`](#fading-the-whole-mix) over the main bus |
| `msg_seedling::ducking::plugin` | the whole app | the ducking envelope alone, if you write your own volume nodes |
| `msg_seedling::fade::plugin` | the whole app | `FadeInAudio`/`FadeOutAudio` alone (already pulled in by the two above) |
| `msg_seedling::device_follow::plugin` | the whole app (native) | [following the OS default output device](#following-the-os-default-output-device) |
| `SeedlingPlugin::<NullBackend>` | the whole app (native, replaces the default backend) | [running audio without a device](#running-audio-without-a-device) |

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

Both axes are drawn at spawn from one system-local `SmallRng`, and the results
are recorded on the entity as `BaseVolume` and `BasePitch` (see [Per-sound
baselines](#per-sound-baselines)) rather than only baked into the volume node
and playback speed. That is what lets a settings change, a damping field, or a
duck scale a randomized sound from *its* level and *its* pitch instead of
flattening every sound of the category onto the same one.

## Spatial Audio

Spatial audio uses seedling's `SpatialPool` with `SpatialBasicNode` (stereo panning + distance attenuation).

You must add a `SpatialListener2D` (or `SpatialListener3D`) to your listener entity:

```rust
// On your camera or player
commands.spawn((Camera2d::default(), SpatialListener2D));
```

Two features look for a listener themselves rather than going through seedling,
and both only recognize `SpatialListener2D`: the listener half of a [damping
field](#sound-damping-fields), and the queue's `.with_max_distance()` culling.
Both fail open in a 3D app -- nothing is damped by listener membership, nothing
is culled -- rather than misbehaving.

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

## Running Audio Without a Device

`NullBackend` is a Firewheel backend with nothing behind it. It renders the
whole graph on its own thread at the stream's real-time rate and discards each
block instead of sending it to the OS, so samplers, buses, filters and fades
all run while nobody hears anything. Use it for tests, bots, headless runs and
machines with no sound card.

Each discarded block is summarized into a `NullOutputMeter` — blocks and frames
rendered, the peak absolute sample, and a count of NaN/infinite samples — so a
test can assert that sound came out and that it was well-formed. Clones share
the same counters: keep one as a resource, hand the other to the stream config.

```rust
let meter = NullOutputMeter::default();
app.insert_resource(meter.clone());
app.add_plugins(SeedlingPlugin::<NullBackend> {
    stream_config: NullBackendConfig { meter, ..default() },
    ..SeedlingPlugin::<NullBackend>::new()
});
```

`device_follow::plugin` is inert under this backend: it only acts on the
`cpal` stream config. Native only.

## Sound Damping Fields

A `SoundDampingField` is a sphere of influence placed on any entity with a
transform -- a body of water, a vent shaft, a wall of foliage. Sound crossing it
is attenuated on three axes at once:

- **volume**, a linear multiplier folded into the sound's volume node;
- **cutoff**, the frequency of the sound's low-pass filter -- the part that
  actually reads as "muffled", because dense matter swallows highs long before
  lows;
- **speed**, a pitch bend on top of the sound's own `BasePitch`.

All three taper from full strength at the centre to nothing at the rim, so a
source drifting out fades back to normal instead of popping.

```rust
app.add_plugins(DampingPlugin::<Sound>::default());

fn flood_the_basement(mut commands: Commands) {
    // Everything within 12 units of the pool sounds like it is under it:
    // most of the level gone, the highs gone first, the pitch dragged down.
    commands.spawn((
        Transform::from_xyz(40.0, -8.0, 0.0),
        SoundDampingField {
            radius: 12.0,
            volume: 0.35,
            cutoff_hz: 700.0,
            speed: 0.9,
            targets: DampingTargets::Both,
        },
    ));
}
```

A field is a *medium*, not a property of the things inside it, so what matters
is whether sound crosses it -- which means two positions, the source's and the
listener's. `DampingTargets` picks which of them a field reads, and the two are
combined with a **maximum**, never a product:

| source | listener | damping |
|--------|----------|---------|
| inside | outside  | full -- the sound has to get out |
| outside| inside   | full -- the sound has to get in |
| inside | inside   | full, **once** -- you are both in the soup |
| outside| outside  | none |

Overlapping fields do not stack either: on each axis the strongest one wins, so
two mild fields cannot silence a sound neither of them authored.

Exemptions come at two levels. `AudioCategory::is_dampable` exempts a whole
category (interface audio is the classic case -- a menu click is not an event in
the world, so a field in the world does not get to muffle it); the
`UndampedSound` marker exempts one sound, which is what a field's own
announcement bed needs. A sound whose volume node another system owns outright
carries `SelfDrivenVolume` and keeps its filter and pitch damping while its
volume is left alone.

**The low-pass axis needs a filter to drive.** It reaches only sounds whose
pool effect chain carries a `LowPassNode`, and `bevy_seedling` fixes that chain
when the pool is created. Route dampable sounds through a pool of your own that
includes one, parked at `OPEN_CUTOFF_HZ`; everything else is volume- and
pitch-damped only.

### Authoring fields in data

Enable the `serde` feature and `SoundDampingField`/`DampingTargets` gain
`Serialize`/`Deserialize`, so a game whose content lives in RON or JSON
deserializes the component itself rather than shadowing its five fields in a
parallel struct:

```toml
msg_seedling = { version = "0.5", features = ["serde"] }
```

```ron
// Every axis defaults to leaving the sound alone, so name only the ones the
// effect is about.
(radius: 40.0, cutoff_hz: 800.0, targets: Listeners)
```

The feature is off by default -- nothing in the crate reads or writes a file
itself, so it costs a `serde` dependency only where a host wants it.

**Geometry is 2D.** Membership is measured in the XY plane, and only
`SpatialListener2D` counts as a listener -- a `SpatialListener3D` app gets the
source half of a field but never the listener half. Measuring in three
dimensions is not a drop-in swap, since a 2D game layers its sprites along `z`
and letting that distance attenuate would muffle sounds by draw order.

## Sidechain Ducking

Raising a crucial cue's own gain to make it land just feeds the saturation that
buries it. A mix makes a cue land the other way around: everything routine
steps back a few decibels the moment it starts, and returns once it has had its
say.

One `DuckingEnvelope` serves the whole mix -- a fast attack, a brief hold, a
slow release, defaulting to a felt-but-safe -6 dB dip. It never touches a
volume node itself; `DampingPlugin` folds it into the same per-frame volume
write damping already owns, because two systems writing the same nodes on
alternate frames would flicker.

Which sounds duck and which spawns trigger the duck are your policy:

```rust
// Routine world sounds carry the marker, decided at spawn.
fn play_ambience(mut commands: Commands, server: Res<AssetServer>) {
    commands.spawn((
        SamplePlayer::new(server.load("wind.ogg")).looping(),
        Sound::Ambience,
        Ducks,
    ));
}

// The cue itself does not -- and it steps the rest of the mix back.
fn play_objective_cue(
    mut commands: Commands,
    server: Res<AssetServer>,
    mut duck: ResMut<DuckingEnvelope>,
) {
    commands.spawn((SamplePlayer::new(server.load("objective.ogg")), Sound::Sfx));
    duck.trigger();
}
```

Trigger it when a qualifying sound *actually spawns* -- a request dropped by a
voice budget should duck nothing. If you write your own volume nodes, read
`DuckingEnvelope::gain_for(ducks)` and fold it in the same way; `ducking::plugin`
registers the envelope without pulling in damping.

## Fading the Whole Mix

`FadeAudio` fades one category and `FadeOutAudio` fades one sound, but a scene
about to despawn its own sound sources has no per-sound list to fade. `FadeMix`
takes the main bus down instead, above every category:

```rust
app.add_plugins(MixFadePlugin::<AudioSettings>::default());

fn leave_the_level(mut commands: Commands) {
    commands.trigger(FadeMix::out(Duration::from_millis(800)));
}

// ...later, from the same caller:
fn enter_the_next_level(mut commands: Commands) {
    commands.trigger(FadeMix::back(Duration::from_millis(800)));
}
```

Ownership is the protocol: whoever engages a fade owns restoring it, so every
`out` is paired with a later `back`. While the mix is pointed at silence, the
config-driven master-volume write stands down -- a volume-slider or mute change
must not snap a faded-out bus back to full -- and the deferred level lands with
the `back`, which reads the live config.

Fades of half a second or longer run in decibel space, because a
linear-amplitude ramp of music length sags audibly down its middle and then
hangs inaudibly on its tail. One consequence is worth knowing: dB interpolation
clamps at a -60 dB floor, so a music-length fade-out idles at linear ~0.001
rather than a true zero. Inaudible on any real mix, but not literally silent.

## Virtual Voice Queue

`PlayAudio` hands a request straight to seedling's pool system. If the pool is
already at its configured size, seedling steals by `SamplePriority` and then by
playback age -- instantly, with no fade, and with no idea which sound is the
one the player actually hears. Two sounds trading places this way is audible
as a hard cut, and a swarm of identical emitters stacks every copy into the mix.

`VirtualVoiceQueuePlugin<C>` puts a ranking layer in front of the pools for
category `C`. Every sound becomes a *virtual* entry ranked by **audible
significance** -- the amplitude it would reach the listener with:

```text
amp(category volume x base volume x damping) x distance gain x priority weight
```

- **Distance gain** comes from the distance model of the pool the entry's lane
  routes into, evaluated game-side by [`attenuation`](#game-side-attenuation)
  against the nearest `SpatialListener2D`, at the same spatial scale the
  audio thread uses. Waiting entries have no audio node at all, so this is the
  only place their loudness can come from.
- **Damping** is the volume a `SoundDampingField` takes off the entry,
  resolved exactly as the damping write resolves it.
- `amp` is firewheel's own mapping from a `VolumeNode`'s linear level to the
  amplitude it applies, so level and distance combine as they do on the audio
  thread.

Only entries that rank inside their budgets get a real `SamplePlayer`, and
entries cross the line via `FadeInAudio`/`FadeOutAudio` instead of a hard cut.
An entry nobody could hear holds no voice. `VirtualSound::with_tier` is a hard
layer above significance: a higher tier always ranks first, however quiet.

### Budgets

Two budgets bound who holds a voice, and both are ranked, never first-come:

- **Per lane.** A `QueuePoolRouter` sorts each entry into a `QueueLane` -- one
  sampler pool -- and each lane holds at most its `max_audible` voices.
- **Per sample.** `VirtualSound::with_max_audible(n)` caps how many entries
  playing the same sample hold a voice at once. Past the cap the *quietest*
  copies wait: a swarm of twenty drones plays its five loudest.

Playing **loops** keep an incumbent bonus (`displacement_margin`, ~2 dB by
default) so two loops of near-equal loudness don't trade places every frame.
Playing **one-shots** get none, and ties go to the newer: under a cap a rapid
stream of shots sheds old tails instead of dropping new hits.

### Lifecycle

A sound lives on one entity for its whole logical lifetime. Spawn
`VirtualSound` directly to keep the entity -- markers, `ChildOf`,
`DespawnOnExit` and host-driven fades survive every promotion and demotion --
or write a `PlayQueuedAudio` for a fire-and-forget sound:

```rust
app.add_plugins(
    VirtualVoiceQueuePlugin::<Sound>::new().with_budget(
        VirtualVoiceBudget::<Sound>::new()
            .with_crossfade(Duration::from_millis(50))
            .with_max_wait(Duration::ZERO),
    ),
);

fn spawn_drone_hum(mut commands: Commands, drone: Entity, hum: Handle<AudioSample>) {
    // Follows the drone, despawns with it, waits silently while five louder
    // copies hold the voices.
    commands.spawn((
        VirtualSound::new(hum, Sound::Sfx)
            .looping()
            .spatial()
            .with_pitch(1.3)
            .with_max_audible(5),
        ChildOf(drone),
        Transform::default(),
    ));
}

fn explode(mut writer: MessageWriter<PlayQueuedAudio<Sound>>, boom: Handle<AudioSample>, at: Vec2) {
    writer.write(PlayQueuedAudio::new(VirtualSound::new(boom, Sound::Sfx).with_tier(2)).at(at));
}
```

- **Admission.** A one-shot passes the admission controls below on its first
  frame or is despawned. A loop is always admitted -- it simply waits.
- **Promotion.** The entry gains a `SamplePlayer` in its lane's pool with the
  bare `C`, `BaseVolume` and `BasePitch`, so damping, ducking and the
  config-driven volume rewrites reach it like any other sound. A fresh sound
  and every one-shot start at full level (a fade-in would blunt the attack); a
  loop coming back after waiting fades in. `VirtualSound::silent_start()`
  leaves the start to the host's own fade or per-frame volume write, and
  `VirtualSound::fixed_pitch()` leaves out the `BasePitch`, so damping never
  bends the voice's speed (music, or a speed the host drives itself).
- **Demotion.** A loop fades out, sheds its voice and waits again (playback
  restarts from the beginning when it returns); a one-shot despawns after its
  fade.
- **Waiting.** Loops wait indefinitely; one-shots give up after
  `VirtualVoiceBudget::max_wait`.

Promoted voices carry `VirtualVoiceBudget::sample_priority` plus their tier as
seedling's `SamplePriority`, and a demoted fader drops to `0`, so if a pool does
run short it is a fader that gets stolen. Size each pool to its lane's
`max_audible` plus the crossfades expected in flight.

Waiting entries carry no `C`, so prefer `StopQueuedAudio` for queue entries:
it is the only stop that also reaches the waiting ones, and it fades audible
ones out through the queue's own crossfade.

### Admission controls

Four opt-in controls drop the one-shots not worth queueing at all, each off
unless set. Loops are never subject to them; a loop past its
`with_max_distance` waits instead of being culled.

```rust
// One global cap, on the budget: at most 64 one-shots enter the queue per
// frame. Excess is dropped, not deferred -- a one-shot delayed a frame would
// play out of sync with its cause.
VirtualVoiceBudget::<Sound>::new().with_max_admissions_per_frame(64);

writer.write(
    PlayQueuedAudio::new(
        VirtualSound::new(ricochet, Sound::Sfx)
            // At most 4 live entries playing this same sample...
            .with_max_concurrent(4)
            // ...at most 2 of them admitted in any one frame...
            .with_max_per_frame(2)
            // ...no sooner than 40ms after the last one...
            .with_min_repeat_interval(Duration::from_millis(40))
            // ...and never when it happens off-screen.
            .with_max_distance(1200.0),
    )
    .at(impact_position),
);
```

A spatial entry whose parent was spawned this frame has no propagated
transform yet; its position counts as unknown, which is never culled and ranks
at full distance gain.

### Lanes

The default router has two 16-voice lanes: spatial entries into seedling's
`SpatialPool`, the rest into `DefaultPool`. A host with its own pools declares
its own lanes -- each with its label, its voice budget, and the
`SpatialBasicNode` its pool spatializes with -- and picks one per entry:

```rust
app.add_plugins(
    VirtualVoiceQueuePlugin::<Sound>::new().with_pool_router(QueuePoolRouter::new(
        vec![
            QueueLane::new(CriticalSpatialPool, 48).with_spatial(spatial_node()),
            QueueLane::new(DampableSpatialPool, 80).with_spatial(spatial_node()),
            QueueLane::new(DefaultPool, 24),
        ],
        |sound| match (sound.is_spatial(), sound.tier() >= 2) {
            (true, true) => 0,
            (true, false) => 1,
            (false, _) => 2,
        },
    )),
);
```

### Bring your own pool

The decision layer is public for hosts that drive their own pools:

- `AdmissionGate` runs the admission controls over the same
  `AdmissionState<C>` resource the plugin uses, against any request shaped
  into an `AdmissionRequest` (`VirtualSound::admission_request` produces one).
- `rank_voices` decides which of a whole set should be audible, with lanes,
  tiers and per-key caps:

```rust
use msg_seedling::{VoiceCandidate, VoiceDecision, rank_voices};

let decisions = rank_voices(&candidates, &lane_capacity);
// For each `VoiceDecision::Promote`: insert your own pool marker + effects
// chain (+ `FadeInAudio` if it should fade in).
// For each `VoiceDecision::Demote`: insert `FadeOutAudio::new(crossfade)`.
```

- `displacement_target` answers the pairwise steal-on-arrival question instead
  -- does this newcomer outrank the weakest voice it may displace? -- with an
  optional hard tier above significance:

```rust
use msg_seedling::{DEFAULT_DISPLACEMENT_MARGIN, DisplacementCandidate, displacement_target};

#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum Tier { Low, Normal, Critical }

// The entity of the voice to steal, or None if the newcomer shouldn't play.
let victim = displacement_target(
    &candidates, // one `DisplacementCandidate` per live voice
    &Tier::Critical,
    newcomer_significance,
    DEFAULT_DISPLACEMENT_MARGIN,
);
```

`FadeInAudio`/`FadeOutAudio` only need `SampleEffects` to work, so they're
usable with any pool, not just the ones this crate spawns.

### Game-side attenuation

`msg_seedling::attenuation` evaluates the spatial distance model outside the
audio thread: `distance_gain` mirrors firewheel's three `DistanceModel`s
(including the clamps firewheel applies to their parameters), `spatial_gain`
adds a `SpatialBasicNode`'s own volume and silence floor, and
`listener_distance` reproduces how `bevy_seedling` turns a world offset into the
node's offset. Anything that has to know how loud a spatial sound arrives --
ranking it, culling it, metering it -- gets the same answer the audio thread
does.

## Architecture

`msg_seedling` is a thin convenience layer over `bevy_seedling`. It does not abstract away the node graph -- power users can use seedling's `Connect`, `SamplerPool`, effects chains, and bus routing directly alongside `msg_seedling`.

### Volume model

A sound's gain is a **product of independently-owned layers**, not a value one
system writes and another overwrites:

```
node volume = category volume x BaseVolume x damping x duck
```

| Factor | Owned by | Lives in |
|--------|----------|----------|
| Master | Your `AudioConfig` | `MainBus` `VolumeNode` (its own node, above the rest) |
| Category | Your `AudioConfig` | recomputed into the product below |
| `BaseVolume` | The `PlayAudio` request: `.with_volume()` x randomization | a component on the sound |
| damping | Whichever `SoundDampingField`s reach it | resolved per frame |
| duck | The mix-wide `DuckingEnvelope` | resolved per frame |

Every system that writes a sound's `VolumeNode` recomputes the whole product
from those components. That is what makes the layers compose: a volume-slider
change scales each sound's own level rather than flattening the category onto
one number, a damping field muffles a quiet ambience bed *relative to how quiet
it already was*, and handing a sound from the damping system back to the
config-driven write is seamless in both directions, because both land on the
same expression with the unused factors at unity.

Two systems never write the same node in one frame. `FadeInAudio`/
`FadeOutAudio` own a node outright for the fade's duration and everything else
stands down; `SelfDrivenVolume` says a host system owns one permanently.

### Per-sound baselines

`BaseVolume` and `BasePitch` are the per-sound halves of that model -- the part
of a sound's gain and speed that belongs to the sound itself, underneath every
layer that scales it. Both are inserted at spawn (by `PlayAudio`'s handler and
by the queue on promotion), so nothing has to reverse-engineer them later.

They are ordinary public components, so a host can read or drive them:

```rust
// Pull one specific loop down by hand, without touching the mix-wide
// envelope -- and have it survive the next volume-slider change.
fn quieten(mut commands: Commands, engine_loop: Entity) {
    commands.entity(engine_loop).insert(BaseVolume(0.3));
}

// Wind an engine up: a damping field still bends this baseline, rather than
// replacing it.
fn rev(mut engines: Query<&mut BasePitch, With<Engine>>, throttle: Res<Throttle>) {
    for mut pitch in &mut engines {
        pitch.0 = 1.0 + throttle.0 * 0.5;
    }
}
```

A sound spawned by hand without them reads as `1.0` on both axes -- the bare
category volume and an unbent pitch. A completed `FadeOutAudio` that keeps its
entity lands on `BaseVolume::SILENT`, so a later config change re-derives the
silence instead of reviving the sound at full volume.

### Voice management

`PlayAudio` is handled entirely by seedling's pool system (`DefaultPool` for
non-spatial, `SpatialPool` for spatial) -- no manual concurrency limits, and
no protection against a hard-cut steal when the pool is full. For voices
ranked by audible loudness under per-pool and per-sample budgets, crossfading
instead of cutting, use `VirtualVoiceQueuePlugin<C>` (see "Virtual Voice
Queue" above).

## Bevy Compatibility

| msg_seedling | bevy_seedling | Bevy |
|-------------|---------------|------|
| 0.6         | 0.7           | 0.18 |
| 0.5         | 0.7           | 0.18 |
| 0.4         | 0.7           | 0.18 |
| 0.3         | 0.7           | 0.18 |
| 0.2         | 0.7           | 0.18 |
| 0.1         | 0.7           | 0.18 |

## License

MIT OR Apache-2.0
