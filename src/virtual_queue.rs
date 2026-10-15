//! Significance-ranked virtual voice queue.
//!
//! A fixed-size pool of real sampler voices (`bevy_seedling`'s own pool
//! machinery) has no idea which currently-playing sound matters most to the
//! player — when it runs out of room it steals by `SamplePriority` and then by
//! playback age, with an instant cut and no fade. This module adds a layer in
//! front of that: every sound becomes a *virtual* entry ranked by **audible
//! significance** — how loud it would actually arrive at the listener — and
//! only the entries that rank inside their budgets ever get a real
//! `SamplePlayer`. When ranking changes — a more significant sound arrives, an
//! emitter moves closer, or a currently-audible one finishes — entries cross
//! the line smoothly via [`FadeInAudio`]/[`FadeOutAudio`] instead of a hard
//! cut.
//!
//! # Significance
//!
//! An entry's significance is the amplitude it would reach the listener with:
//!
//! ```text
//! amp(category volume × base volume × damping) × distance gain × priority weight
//! ```
//!
//! `amp` is firewheel's own mapping from the linear level a `VolumeNode` is
//! written with to the amplitude it applies (`Volume::amp`), so a sound's
//! level and its distance gain combine the way they do on the audio thread.
//!
//! - **Distance gain** is evaluated for spatial entries from the distance
//!   model of the pool their lane routes into ([`QueueLane::with_spatial`]),
//!   with [`attenuation`](crate::attenuation) — the same curve the audio thread
//!   applies, against the nearest `SpatialListener2D`, at the same spatial
//!   scale. Entries without a voice have no audio node at all, so this is the
//!   only place their loudness can come from.
//! - **Damping** is the volume a [`SoundDampingField`] takes off the entry,
//!   resolved exactly as [`apply_sound_damping`](crate::apply_sound_damping)
//!   resolves it, for dampable categories and sounds not marked
//!   [`UndampedSound`]. The ducking envelope is left out: it scales the whole
//!   routine mix at once and only for a beat, so ranking through it would only
//!   churn voices.
//!
//! A [`VirtualSound::with_tier`] is a hard layer above significance: an entry
//! of a higher tier always ranks above every entry of a lower one, however
//! quiet. An entry nobody could hear — zero significance — never holds a
//! voice.
//!
//! # Budgets
//!
//! Two budgets bound who may hold a voice, and both are ranked, never
//! first-come:
//!
//! - **Per lane.** The [`QueuePoolRouter`] sorts every entry into a
//!   [`QueueLane`] — one sampler pool — and each lane holds at most
//!   [`QueueLane::max_audible`] voices.
//! - **Per sample.** [`VirtualSound::with_max_audible`] caps how many entries
//!   playing the same sample hold a voice at once. Past the cap the quietest
//!   copies are the ones without a voice, so ten drones in a swarm play their
//!   five loudest — not their five oldest.
//!
//! Playing *loops* keep a small incumbent bonus
//! ([`VirtualVoiceBudget::displacement_margin`]): a newcomer has to be
//! noticeably louder to take a loop's voice, which keeps two loops of near
//! equal loudness from trading places every frame. Playing *one-shots* get no
//! such bonus, and a tie goes to the newer one: the oldest copy is deep in its
//! tail, the new one is its attack, so under a cap a rapid stream of shots
//! sheds old tails rather than dropping new hits.
//!
//! # Voice lifecycle
//!
//! A sound lives on one entity for its whole logical lifetime, with or without
//! a voice. Spawn it directly — `commands.spawn((VirtualSound::new(..), ..))`,
//! with whatever markers, `ChildOf` and fades the host wants on it — or write a
//! [`PlayQueuedAudio`] for a fire-and-forget sound.
//!
//! - **Admission.** A one-shot passes [`AdmissionGate`]'s controls on its first
//!   frame or is despawned; a loop is always admitted — it simply waits
//!   until it ranks.
//! - **Promotion.** The entry gains a `SamplePlayer` routed into its lane's
//!   pool, carrying the bare `C` component, [`BaseVolume`] and [`BasePitch`],
//!   so the crate's per-sound category, damping and ducking systems reach it
//!   like any other sound. A sound promoted on the frame it was admitted, and
//!   every one-shot, starts at full level — a fade-in would blunt its attack.
//!   A loop that comes back after waiting fades in over the crossfade.
//!   [`VirtualSound::silent_start`] leaves the start to the host: the voice
//!   starts silent and the host's own fade or per-frame write brings it up.
//!   [`VirtualSound::fixed_pitch`] leaves out the [`BasePitch`], so damping
//!   never bends that voice's speed.
//! - **Demotion.** Looping entries fade out, shed their `SamplePlayer`, and
//!   return to the silent virtual state, eligible for re-promotion; playback
//!   restarts from the beginning when re-promoted. One-shot entries despawn
//!   once their demotion fade completes — a partially-played one-shot
//!   restarting later would be wrong.
//! - **Waiting.** Loops wait indefinitely. One-shots give up after
//!   [`VirtualVoiceBudget::max_wait`] — a one-shot promoted long after its
//!   cause would play out of sync with it.
//!
//! A promoted voice that ends inside seedling — stolen, expired in the sampler
//! queue, or (for one-shots) finished playing — is reclaimed under the same
//! policy. Promoted voices carry [`VirtualVoiceBudget::sample_priority`] plus
//! their tier, and a demoted fader drops to `SamplePriority(0)`, so when a pool
//! does run short it is a fader the sampler steals. Size each pool to hold its
//! lane's `max_audible` plus the crossfades expected in flight.
//!
//! Waiting entries carry no `C`, and a demoted loop sheds it along with its
//! voice, so prefer [`StopQueuedAudio`] for queue entries: it is the only stop
//! that also reaches the waiting ones, and it fades audible ones out through
//! the queue's own crossfade instead of cutting them.
//!
//! # Bring your own pool
//!
//! The queue's decision layer is public for hosts that drive their own sampler
//! pools instead of adding [`VirtualVoiceQueuePlugin`]:
//!
//! - [`AdmissionGate`] runs the admission controls — the global per-frame
//!   budget plus [`AdmissionRequest`]'s per-key caps, repeat interval and
//!   distance cull — over the same [`AdmissionState`] resource the plugin uses.
//! - [`rank_voices`] answers "which of this whole set should be audible", with
//!   lanes, tiers and per-key caps.
//! - [`displacement_target`] answers the pairwise question instead — "does
//!   this newcomer outrank the weakest voice it may displace" — with an
//!   optional hard tier ordering above significance.
//!
//! # Example
//!
//! ```
//! use bevy::prelude::*;
//! use msg_seedling::prelude::*;
//!
//! #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
//! #[reflect(Component)]
//! enum Sound {
//!     #[default]
//!     Sfx,
//!     Ambience,
//! }
//!
//! #[derive(Resource, Clone, Default)]
//! struct AudioSettings;
//!
//! impl AudioConfig for AudioSettings {
//!     fn master_volume(&self) -> f32 {
//!         1.0
//!     }
//! }
//!
//! impl AudioCategory for Sound {
//!     type Config = AudioSettings;
//!     fn volume(&self, _config: &AudioSettings) -> f32 {
//!         1.0
//!     }
//! }
//!
//! #[derive(Component)]
//! struct Hum;
//!
//! fn play_sounds(mut commands: Commands, mut writer: MessageWriter<PlayQueuedAudio<Sound>>) {
//!     // A looping hum the caller keeps a handle on: waits as virtual while
//!     // louder things hold the voices, promoted when it ranks.
//!     commands.spawn((
//!         VirtualSound::new(Handle::default(), Sound::Ambience)
//!             .looping()
//!             .with_volume(0.2),
//!         Hum,
//!     ));
//!
//!     // A fire-and-forget explosion, at most four of them at once: outranks
//!     // quieter voices and displaces the weakest copy beyond the cap.
//!     writer.write(
//!         PlayQueuedAudio::new(
//!             VirtualSound::new(Handle::default(), Sound::Sfx).with_max_audible(4),
//!         )
//!         .at(Vec2::new(100.0, 0.0)),
//!     );
//! }
//!
//! let mut app = App::new();
//! app.add_plugins(MinimalPlugins);
//! app.add_plugins(VirtualVoiceQueuePlugin::<Sound>::default());
//! app.add_systems(Update, play_sounds);
//! app.update();
//! ```

use core::cmp::Ordering;
use core::hash::Hash;
use core::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use core::time::Duration;
use std::sync::Arc;

use bevy::ecs::entity::Entities;
use bevy::ecs::system::EntityCommands;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use bevy_seedling::pool::label::PoolLabelContainer;
use bevy_seedling::pool::{CompletionReason, Sampler};
use bevy_seedling::prelude::*;
// Explicit: with bevy's `bevy_audio` feature on, `bevy::prelude` exports a
// `PlaybackSettings` of its own and the two globs would otherwise resolve to it.
use bevy_seedling::prelude::PlaybackSettings;
use bevy_seedling::sample::{AudioSample, QueuedSample};

use crate::attenuation::{nearest_listener_distance, spatial_gain};
use crate::baseline::{BasePitch, BaseVolume, sanitize_weight};
use crate::damping::{
    ActiveField, SoundDamping, SoundDampingField, UndampedSound, nearest_listener,
};
use crate::fade::{FadeInAudio, FadeOutAudio, FadeSystems};
use crate::messages::SpatialPosition;
use crate::traits::AudioCategory;

const DEFAULT_LANE_VOICES: usize = 16;
const DEFAULT_CROSSFADE: Duration = Duration::from_millis(50);
const DEFAULT_MAX_WAIT: Duration = Duration::from_millis(500);
const DEFAULT_SAMPLE_PRIORITY: i32 = 2;

/// Default incumbent-bonus displacement margin (~2 dB): a newcomer must be
/// this factor more significant than an audible loop to displace it. Used
/// by [`VirtualVoiceBudget::displacement_margin`], and the value to reach
/// for as [`displacement_target`]'s `margin` unless you have a reason not
/// to.
pub const DEFAULT_DISPLACEMENT_MARGIN: f32 = 1.25;

/// Timing and priority knobs for one category type's virtual voice queue.
///
/// Scoped per `C` so unrelated category types (e.g. music vs. SFX) never
/// share state when both use [`VirtualVoiceQueuePlugin`]. How many voices a
/// queue may hold is the lanes' business — see [`QueuePoolRouter`].
#[derive(Resource, Clone, Debug)]
pub struct VirtualVoiceBudget<C: AudioCategory> {
    /// Duration of the crossfade applied when an entry is displaced or a
    /// waiting loop is promoted.
    pub crossfade: Duration,
    /// How long a never-promoted one-shot entry waits before it is dropped.
    /// Looping entries wait indefinitely. `Duration::ZERO` drops a one-shot
    /// that does not rank on the frame it was requested.
    pub max_wait: Duration,
    /// Seedling `SamplePriority` given to promoted voices, before their tier
    /// is added, so default-priority (0) one-shots sharing the pool cannot
    /// steal them.
    pub sample_priority: i32,
    /// Global cap on how many one-shots may be admitted per frame, across
    /// every sound. `None` (the default) admits everything. One-shots past the
    /// cap are dropped, not deferred — a one-shot delayed a frame would play
    /// out of sync with its cause, and its sender can re-request. Loops are
    /// never counted.
    pub max_admissions_per_frame: Option<usize>,
    /// Incumbent bonus for playing loops: a newcomer must exceed an audible
    /// loop's significance by this factor to displace it. The default `1.25`
    /// (~2 dB) is hysteresis against crossfade flutter when two loops hover
    /// near equal loudness — without it an epsilon-louder newcomer churns
    /// demotions, and each loop demotion restarts playback from the
    /// beginning. Sanitized to `>= 1.0` (`1.0` = no hysteresis).
    pub displacement_margin: f32,
    _phantom: core::marker::PhantomData<C>,
}

impl<C: AudioCategory> VirtualVoiceBudget<C> {
    /// The default budget: 50ms crossfade, 500ms max wait, sample priority 2,
    /// displacement margin 1.25, no per-frame admission cap.
    #[must_use]
    pub fn new() -> Self {
        Self {
            crossfade: DEFAULT_CROSSFADE,
            max_wait: DEFAULT_MAX_WAIT,
            sample_priority: DEFAULT_SAMPLE_PRIORITY,
            max_admissions_per_frame: None,
            displacement_margin: DEFAULT_DISPLACEMENT_MARGIN,
            _phantom: core::marker::PhantomData,
        }
    }

    /// Sets the promotion/demotion crossfade duration.
    #[must_use]
    pub fn with_crossfade(mut self, crossfade: Duration) -> Self {
        self.crossfade = crossfade;
        self
    }

    /// Sets how long a never-promoted one-shot waits before being dropped.
    #[must_use]
    pub fn with_max_wait(mut self, max_wait: Duration) -> Self {
        self.max_wait = max_wait;
        self
    }

    /// Sets the seedling `SamplePriority` given to promoted voices.
    #[must_use]
    pub fn with_sample_priority(mut self, sample_priority: i32) -> Self {
        self.sample_priority = sample_priority;
        self
    }

    /// Caps how many one-shots may be admitted per frame.
    #[must_use]
    pub fn with_max_admissions_per_frame(mut self, cap: usize) -> Self {
        self.max_admissions_per_frame = Some(cap);
        self
    }

    /// Sets the incumbent-bonus displacement margin, sanitized to `>= 1.0`.
    #[must_use]
    pub fn with_displacement_margin(mut self, displacement_margin: f32) -> Self {
        self.displacement_margin = sanitize_margin(displacement_margin);
        self
    }
}

impl<C: AudioCategory> Default for VirtualVoiceBudget<C> {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps a margin to `>= 1.0`, with a non-finite one meaning no hysteresis.
fn sanitize_margin(margin: f32) -> f32 {
    if margin.is_finite() {
        margin.max(1.0)
    } else {
        1.0
    }
}

/// Hands out [`VirtualSound`] creation order, which breaks significance ties.
static NEXT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A sound tracked by the virtual voice queue.
///
/// Lives on one entity for the sound's whole logical lifetime: present
/// without [`Audible`] while virtual (no real voice, nothing plays), present
/// with [`Audible`] once promoted. Spawn it directly to keep the entity — the
/// queue never touches components it did not insert, so markers, `ChildOf`,
/// `DespawnOnExit` and host-driven fades survive every promotion and
/// demotion — or send it in a [`PlayQueuedAudio`].
///
/// A spatial entry ([`Self::spatial`]) plays from its own transform: give it
/// a `Transform` (and a `ChildOf` to follow an emitter); one without gets a
/// default `Transform` on admission.
#[derive(Component, Clone, Reflect)]
#[reflect(Component)]
pub struct VirtualSound<C: AudioCategory> {
    handle: Handle<AudioSample>,
    category: C,
    looping: bool,
    spatial: bool,
    /// Request volume before the category multiplier; the effective target
    /// is recomputed from `C::Config` every frame.
    base_volume: f32,
    pitch: f32,
    priority: f32,
    tier: i32,
    max_audible: Option<usize>,
    max_distance: Option<f32>,
    silent_start: bool,
    fixed_pitch: bool,
    max_concurrent: Option<usize>,
    max_per_frame: Option<usize>,
    min_repeat_interval: Option<Duration>,
    #[reflect(ignore)]
    state: EntryState,
}

/// The queue's own bookkeeping on an entry.
#[derive(Clone, Copy, Debug, Default)]
struct EntryState {
    /// The lane the router chose, set on admission; `None` until admitted.
    lane: Option<usize>,
    /// When the entry was admitted.
    admitted_at: Duration,
    /// Creation order; a higher number is newer.
    sequence: u64,
    /// Whether the entry has held a voice before.
    promoted: bool,
}

impl<C: AudioCategory> VirtualSound<C> {
    /// A one-shot, flat sound of `handle` in `category` at unit volume, pitch
    /// and priority weight, tier `0`, with every cap and control off.
    #[must_use]
    pub fn new(handle: Handle<AudioSample>, category: C) -> Self {
        Self {
            handle,
            category,
            looping: false,
            spatial: false,
            base_volume: 1.0,
            pitch: 1.0,
            priority: 1.0,
            tier: 0,
            max_audible: None,
            max_distance: None,
            silent_start: false,
            fixed_pitch: false,
            max_concurrent: None,
            max_per_frame: None,
            min_repeat_interval: None,
            state: EntryState {
                sequence: NEXT_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed),
                ..Default::default()
            },
        }
    }

    /// Loops endlessly once promoted, and waits instead of being dropped
    /// while it does not rank.
    #[must_use]
    pub fn looping(mut self) -> Self {
        self.looping = true;
        self
    }

    /// Plays from the entity's own transform, through a spatial lane.
    #[must_use]
    pub fn spatial(mut self) -> Self {
        self.spatial = true;
        self
    }

    /// Sets the base volume multiplier (before category and master), sanitized
    /// like every weight: non-finite becomes `0.0`, negative clamps to `0.0`.
    #[must_use]
    pub fn with_volume(mut self, volume: f32) -> Self {
        self.base_volume = sanitize_weight(volume);
        self
    }

    /// Sets the playback speed baseline, which damping bends against. A
    /// non-finite or non-positive speed has no sensible pitch and becomes
    /// `1.0`.
    #[must_use]
    pub fn with_pitch(mut self, pitch: f32) -> Self {
        self.pitch = BasePitch::new(pitch).0;
        self
    }

    /// Sets the priority weight multiplying audible significance, sanitized
    /// like [`Self::with_volume`]. Use it for game-meaning importance a raw
    /// volume number can't express; use [`Self::with_tier`] when the
    /// importance must win outright.
    #[must_use]
    pub fn with_priority(mut self, priority: f32) -> Self {
        self.priority = sanitize_weight(priority);
        self
    }

    /// Places the entry in a hard priority tier: every entry of a higher tier
    /// ranks above every entry of a lower one, however quiet. Also raises the
    /// promoted voice's `SamplePriority` by the same amount.
    #[must_use]
    pub fn with_tier(mut self, tier: i32) -> Self {
        self.tier = tier;
        self
    }

    /// Caps how many entries playing this same sample hold a voice at once;
    /// past the cap the least significant copies wait (loops) or are dropped
    /// (one-shots).
    #[must_use]
    pub fn with_max_audible(mut self, max_audible: usize) -> Self {
        self.max_audible = Some(max_audible);
        self
    }

    /// Farthest this entry may be from the nearest `SpatialListener2D`, in
    /// world units in the XY plane, and still hold a voice. A one-shot beyond
    /// it is culled on admission; a loop beyond it waits. Never applies
    /// without a listener or to a non-spatial entry. Sanitized: a non-finite
    /// distance is discarded, a negative one clamps to `0.0`.
    #[must_use]
    pub fn with_max_distance(mut self, max_distance: f32) -> Self {
        self.max_distance = max_distance.is_finite().then_some(max_distance.max(0.0));
        self
    }

    /// Starts every promoted voice silent and leaves bringing it up to the
    /// host — a fade of its own, or a system that writes the volume node every
    /// frame.
    #[must_use]
    pub fn silent_start(mut self) -> Self {
        self.silent_start = true;
        self
    }

    /// Keeps the voice at its own pitch: it is promoted without a
    /// [`BasePitch`], so no [`SoundDampingField`] bends its speed. For sounds
    /// whose pitch is a musical fact rather than an acoustic one, and for
    /// voices whose speed a host system drives itself.
    #[must_use]
    pub fn fixed_pitch(mut self) -> Self {
        self.fixed_pitch = true;
        self
    }

    /// Admission cap for one-shots: the request is dropped when this many
    /// entries playing the same sample already exist, voiced or waiting.
    #[must_use]
    pub fn with_max_concurrent(mut self, max_concurrent: usize) -> Self {
        self.max_concurrent = Some(max_concurrent);
        self
    }

    /// Admission cap for one-shots: at most this many of the same sample are
    /// admitted per frame.
    #[must_use]
    pub fn with_max_per_frame(mut self, max_per_frame: usize) -> Self {
        self.max_per_frame = Some(max_per_frame);
        self
    }

    /// Admission control for one-shots: the minimum time since this same
    /// sample was last admitted, measured against previous admissions that
    /// themselves set an interval.
    #[must_use]
    pub fn with_min_repeat_interval(mut self, interval: Duration) -> Self {
        self.min_repeat_interval = Some(interval);
        self
    }

    /// Replaces the base volume, e.g. after the host redrew it from a
    /// hot-reloaded config. Sanitized like [`Self::with_volume`]; takes effect
    /// at the next ranking and the next promotion.
    pub fn set_base_volume(&mut self, volume: f32) {
        self.base_volume = sanitize_weight(volume);
    }

    /// The category this sound was requested under.
    #[must_use]
    pub fn category(&self) -> C {
        self.category
    }

    /// Whether this sound loops once promoted.
    #[must_use]
    pub fn is_looping(&self) -> bool {
        self.looping
    }

    /// Whether this sound plays from its entity's transform.
    #[must_use]
    pub fn is_spatial(&self) -> bool {
        self.spatial
    }

    /// The sample this sound plays.
    #[must_use]
    pub fn handle(&self) -> &Handle<AudioSample> {
        &self.handle
    }

    /// The base volume multiplier.
    #[must_use]
    pub fn base_volume(&self) -> f32 {
        self.base_volume
    }

    /// The playback speed baseline.
    #[must_use]
    pub fn pitch(&self) -> f32 {
        self.pitch
    }

    /// The priority weight.
    #[must_use]
    pub fn priority(&self) -> f32 {
        self.priority
    }

    /// The hard priority tier.
    #[must_use]
    pub fn tier(&self) -> i32 {
        self.tier
    }

    /// The per-sample voice cap.
    #[must_use]
    pub fn max_audible(&self) -> Option<usize> {
        self.max_audible
    }

    /// The lane the router placed this entry in, once admitted.
    #[must_use]
    pub fn lane(&self) -> Option<usize> {
        self.state.lane
    }

    /// The admission-control slice of this entry, measured at `position`, in
    /// the shape [`AdmissionGate::try_admit`] takes.
    #[must_use]
    pub fn admission_request(&self, position: Option<Vec2>) -> AdmissionRequest {
        AdmissionRequest {
            key: self.handle.id(),
            position,
            max_concurrent: self.max_concurrent,
            max_per_frame: self.max_per_frame,
            min_repeat_interval: self.min_repeat_interval,
            max_distance: self.max_distance,
        }
    }
}

/// Marker: this entry currently owns a live `SamplePlayer`.
#[derive(Component, Default, Reflect)]
#[reflect(Component)]
#[component(storage = "SparseSet")]
pub struct Audible;

/// Marker: this entry is fading out after a demotion or stop; excluded from
/// ranking while in this state.
#[derive(Component, Default, Reflect)]
#[reflect(Component)]
#[component(storage = "SparseSet")]
pub struct Retiring;

/// Message: play a fire-and-forget sound through the queue.
///
/// Spawns the [`VirtualSound`] on an entity of its own: under `parent`
/// (`ChildOf`, so it follows the emitter and despawns with it) or at a fixed
/// `position`. Either makes the sound spatial. A request whose parent no
/// longer exists is dropped — a spatial sound of a dead emitter must not
/// play at the origin.
#[derive(Message, Clone)]
pub struct PlayQueuedAudio<C: AudioCategory> {
    /// The sound to play.
    pub sound: VirtualSound<C>,
    /// The entity the sound follows.
    pub parent: Option<Entity>,
    /// The world position the sound plays at.
    pub position: Option<SpatialPosition>,
}

impl<C: AudioCategory> PlayQueuedAudio<C> {
    /// Wraps a sound in a request.
    #[must_use]
    pub fn new(sound: VirtualSound<C>) -> Self {
        Self {
            sound,
            parent: None,
            position: None,
        }
    }

    /// Attaches to a parent entity via `ChildOf`. Implies spatial audio.
    #[must_use]
    pub fn with_parent(mut self, parent: Entity) -> Self {
        self.parent = Some(parent);
        self.sound.spatial = true;
        self
    }

    /// Plays at a world position. Implies spatial audio.
    #[must_use]
    pub fn at(mut self, position: impl Into<SpatialPosition>) -> Self {
        self.position = Some(position.into());
        self.sound.spatial = true;
        self
    }
}

impl<C: AudioCategory> From<VirtualSound<C>> for PlayQueuedAudio<C> {
    fn from(sound: VirtualSound<C>) -> Self {
        Self::new(sound)
    }
}

/// Message: stop entries in the virtual voice queue for category type `C`.
///
/// Audible entries fade out over the budget's crossfade and despawn; virtual
/// (waiting) entries despawn immediately; already-retiring entries are
/// switched to despawn when their fade completes. Only entries existing at
/// the start of the frame are stopped: a [`PlayQueuedAudio`] written the
/// same frame spawns its entry after stop handling, so the just-requested
/// sound plays on.
#[derive(Message, Clone)]
pub struct StopQueuedAudio<C: AudioCategory> {
    /// The category to stop. `None` = stop every queued entry of type `C`.
    pub category: Option<C>,
    /// Only stop entries playing this sample. `None` = any sample.
    pub handle: Option<Handle<AudioSample>>,
}

impl<C: AudioCategory> StopQueuedAudio<C> {
    /// Stops all queued entries matching a specific category.
    #[must_use]
    pub fn category(category: C) -> Self {
        Self {
            category: Some(category),
            handle: None,
        }
    }

    /// Stops all queued entries of category type `C`.
    #[must_use]
    pub fn all() -> Self {
        Self {
            category: None,
            handle: None,
        }
    }

    /// Restricts the stop to entries playing `handle`.
    #[must_use]
    pub fn with_handle(mut self, handle: Handle<AudioSample>) -> Self {
        self.handle = Some(handle);
        self
    }
}

type LabelHook = Arc<dyn Fn(&mut EntityCommands) + Send + Sync>;

/// One sampler pool the queue promotes into, and the voices it may hold.
///
/// `route` runs at promotion, on the entry about to receive its
/// `SamplePlayer`, and inserts the pool label; `release` runs when the voice
/// is released and removes it again, so the entry returns to a bare virtual
/// state.
#[derive(Clone)]
pub struct QueueLane {
    max_audible: usize,
    spatial: Option<SpatialBasicNode>,
    route: LabelHook,
    release: LabelHook,
}

impl QueueLane {
    /// A lane promoting into the pool labelled `label`, holding at most
    /// `max_audible` voices. Flat until [`Self::with_spatial`] says otherwise.
    #[must_use]
    pub fn new<L: Component + Clone>(label: L, max_audible: usize) -> Self {
        Self {
            max_audible,
            spatial: None,
            route: Arc::new(move |ec: &mut EntityCommands| {
                ec.insert(label.clone());
            }),
            release: Arc::new(|ec: &mut EntityCommands| {
                ec.remove::<L>();
            }),
        }
    }

    /// Declares the spatialization node the lane's pool runs, so ranking
    /// evaluates the same distance model the audio thread applies. Pass the
    /// node the pool was built with.
    #[must_use]
    pub fn with_spatial(mut self, node: SpatialBasicNode) -> Self {
        self.spatial = Some(node);
        self
    }

    /// How many voices this lane may hold at once.
    #[must_use]
    pub fn max_audible(&self) -> usize {
        self.max_audible
    }

    /// The spatialization node ranking evaluates distance through, if any.
    #[must_use]
    pub fn spatial_node(&self) -> Option<&SpatialBasicNode> {
        self.spatial.as_ref()
    }
}

type LaneSelect<C> = Arc<dyn Fn(&VirtualSound<C>) -> usize + Send + Sync>;

/// Where promoted voices go: the lanes behind [`VirtualVoiceQueuePlugin`],
/// pluggable per category type.
///
/// `select` runs once per entry, on admission, and returns the index of the
/// entry's lane in `lanes`; an out-of-range index drops the entry with an
/// error. The default router has two lanes of 16 voices: spatial entries
/// into seedling's `SpatialPool` (whose default `SpatialBasicNode` it
/// declares), the rest into `DefaultPool`.
#[derive(Resource, Clone)]
pub struct QueuePoolRouter<C: AudioCategory> {
    lanes: Vec<QueueLane>,
    select: LaneSelect<C>,
}

impl<C: AudioCategory> QueuePoolRouter<C> {
    /// Creates a router over `lanes`, choosing each entry's lane with
    /// `select`.
    pub fn new(
        lanes: Vec<QueueLane>,
        select: impl Fn(&VirtualSound<C>) -> usize + Send + Sync + 'static,
    ) -> Self {
        Self {
            lanes,
            select: Arc::new(select),
        }
    }

    /// The lanes, by index.
    #[must_use]
    pub fn lanes(&self) -> &[QueueLane] {
        &self.lanes
    }
}

impl<C: AudioCategory> Default for QueuePoolRouter<C> {
    fn default() -> Self {
        Self::new(
            vec![
                QueueLane::new(SpatialPool, DEFAULT_LANE_VOICES)
                    .with_spatial(SpatialBasicNode::default()),
                QueueLane::new(DefaultPool, DEFAULT_LANE_VOICES),
            ],
            |sound| usize::from(!sound.is_spatial()),
        )
    }
}

/// The queue's per-frame systems, in order: voice reclaim and fade
/// completion, request intake and stops, admission, ranking. Runs after
/// [`FadeSystems`]. A host spawning [`VirtualSound`] entities orders its
/// system before this set for them to rank the same frame.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VirtualQueueSystems;

/// Plugin adding a significance-ranked virtual voice queue for category `C`.
///
/// Independent of [`MsgSeedlingPlugin`](crate::MsgSeedlingPlugin) — add both
/// if you want direct [`PlayAudio`](crate::PlayAudio) playback *and* the
/// queue for the same category type. Pulls in [`crate::fade::plugin`]
/// automatically (safe to add alongside an explicit `fade::plugin`, or
/// alongside another category's `VirtualVoiceQueuePlugin` — registration is
/// idempotent).
pub struct VirtualVoiceQueuePlugin<C: AudioCategory> {
    budget: VirtualVoiceBudget<C>,
    pool_router: QueuePoolRouter<C>,
}

impl<C: AudioCategory> Default for VirtualVoiceQueuePlugin<C> {
    fn default() -> Self {
        Self {
            budget: VirtualVoiceBudget::default(),
            pool_router: QueuePoolRouter::default(),
        }
    }
}

impl<C: AudioCategory> VirtualVoiceQueuePlugin<C> {
    /// Creates a new plugin with the default budget and router.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the timing and priority budget.
    #[must_use]
    pub fn with_budget(mut self, budget: VirtualVoiceBudget<C>) -> Self {
        self.budget = budget;
        self
    }

    /// Sets the lanes promoted voices go to, replacing the default
    /// `SpatialPool`/`DefaultPool` pair.
    #[must_use]
    pub fn with_pool_router(mut self, pool_router: QueuePoolRouter<C>) -> Self {
        self.pool_router = pool_router;
        self
    }
}

impl<C: AudioCategory> Plugin for VirtualVoiceQueuePlugin<C> {
    fn build(&self, app: &mut App) {
        crate::fade::plugin(app);

        app.register_type::<VirtualSound<C>>();
        app.register_type::<Audible>();
        app.register_type::<Retiring>();
        crate::baseline::register_types(app);
        app.init_resource::<C::Config>();
        app.init_resource::<AdmissionState<C>>();
        app.insert_resource(self.budget.clone());
        app.insert_resource(self.pool_router.clone());
        app.add_message::<PlayQueuedAudio<C>>();
        app.add_message::<StopQueuedAudio<C>>();
        app.configure_sets(Update, VirtualQueueSystems.after(FadeSystems));
        app.add_systems(
            Update,
            (
                (
                    reclaim_lost_voices::<C>,
                    finish_demotions::<C>,
                    enqueue_queued_audio::<C>,
                    handle_stop_queued_audio::<C>,
                ),
                admit_virtual_sounds::<C>,
                rank_virtual_voices::<C>,
            )
                .chain()
                .in_set(VirtualQueueSystems),
        );
    }
}

/// Per-key admission bookkeeping: when each key was last admitted, for
/// [`AdmissionRequest::min_repeat_interval`].
///
/// `M` is a marker for [`Resource`] discrimination (e.g. one per audio
/// category); `K` is the key type per-key controls track on. Both default
/// for the built-in queue; a bring-your-own-pool host picks the types that
/// match its request shape.
///
/// Bounded however many distinct samples stream through a session: only
/// admissions that carry an interval are recorded, and once the map grows
/// past a threshold, entries older than the longest interval seen — too old
/// to ever block a request again — are pruned before recording more.
#[derive(Resource)]
pub struct AdmissionState<M = (), K = AssetId<AudioSample>> {
    last_admitted: bevy::platform::collections::HashMap<K, Duration>,
    longest_interval: Duration,
    _phantom: core::marker::PhantomData<M>,
}

impl<M, K> Default for AdmissionState<M, K> {
    fn default() -> Self {
        Self {
            last_admitted: Default::default(),
            longest_interval: Duration::ZERO,
            _phantom: core::marker::PhantomData,
        }
    }
}

impl<M, K: Eq + Hash + Clone> AdmissionState<M, K> {
    const PRUNE_THRESHOLD: usize = 64;

    fn record(&mut self, id: K, now: Duration, interval: Duration) {
        self.longest_interval = self.longest_interval.max(interval);
        if self.last_admitted.len() >= Self::PRUNE_THRESHOLD
            && !self.last_admitted.contains_key(&id)
        {
            let horizon = self.longest_interval;
            self.last_admitted
                .retain(|_, last| now.saturating_sub(*last) < horizon);
        }
        self.last_admitted.insert(id, now);
    }
}

/// The admission-control slice of one request, decoupled from
/// [`VirtualSound`] so a bring-your-own-pool host can gate its own
/// request type through [`AdmissionGate`]. Every control defaults to off — a
/// request using none of them is always admitted (up to the gate's global
/// frame budget).
///
/// `K` is the key type that per-key controls (per-frame cap, concurrency,
/// repeat interval) track on — [`AssetId<AudioSample>`] for the built-in
/// queue, but any `Hash + Eq + Clone` type for a bring-your-own-pool host.
///
/// [`VirtualSound::admission_request`] produces one from a queue entry;
/// each field mirrors the [`VirtualSound`] builder of the same name.
#[derive(Clone, Debug)]
pub struct AdmissionRequest<K = AssetId<AudioSample>> {
    /// The key identifying the sound being requested; per-key controls
    /// track on this.
    pub key: K,
    /// XY world position, measured against listeners for
    /// [`Self::max_distance`]. `None` is never distance-culled.
    pub position: Option<Vec2>,
    /// Cap on live entries playing this same key; see
    /// [`VirtualSound::with_max_concurrent`].
    pub max_concurrent: Option<usize>,
    /// Cap on admissions of this same key within one frame; see
    /// [`VirtualSound::with_max_per_frame`].
    pub max_per_frame: Option<usize>,
    /// Minimum time since this same key was last admitted *with an
    /// interval*; see [`VirtualSound::with_min_repeat_interval`].
    pub min_repeat_interval: Option<Duration>,
    /// Maximum distance from the nearest listener worth admitting at; see
    /// [`VirtualSound::with_max_distance`].
    pub max_distance: Option<f32>,
}

impl<K> AdmissionRequest<K> {
    /// Creates a request for `key` with every control off.
    #[must_use]
    pub fn new(key: K) -> Self {
        Self {
            key,
            position: None,
            max_concurrent: None,
            max_per_frame: None,
            min_repeat_interval: None,
            max_distance: None,
        }
    }

    /// Sets the XY position [`Self::max_distance`] measures from.
    #[must_use]
    pub fn at(mut self, position: Vec2) -> Self {
        self.position = Some(position);
        self
    }

    /// Caps live entries playing this same key.
    #[must_use]
    pub fn with_max_concurrent(mut self, max_concurrent: usize) -> Self {
        self.max_concurrent = Some(max_concurrent);
        self
    }

    /// Caps admissions of this same key within one frame.
    #[must_use]
    pub fn with_max_per_frame(mut self, max_per_frame: usize) -> Self {
        self.max_per_frame = Some(max_per_frame);
        self
    }

    /// Sets the minimum time since this same key was last admitted.
    #[must_use]
    pub fn with_min_repeat_interval(mut self, interval: Duration) -> Self {
        self.min_repeat_interval = Some(interval);
        self
    }

    /// Culls the request when farther than this from the nearest listener.
    /// Sanitized like [`VirtualSound::with_max_distance`]: a non-finite
    /// distance is discarded (never culls), a negative one clamps to `0.0`.
    #[must_use]
    pub fn with_max_distance(mut self, max_distance: f32) -> Self {
        self.max_distance = max_distance.is_finite().then_some(max_distance.max(0.0));
        self
    }
}

/// Why [`AdmissionGate::try_admit`] rejected a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionRejection {
    /// The gate's global per-frame budget is spent
    /// ([`VirtualVoiceBudget::max_admissions_per_frame`]).
    FrameBudgetSpent,
    /// This key already hit its [`AdmissionRequest::max_per_frame`] this
    /// frame.
    PerFrameCap,
    /// This key already has [`AdmissionRequest::max_concurrent`] live
    /// entries (same-frame admissions included).
    ConcurrentCap,
    /// This key was last admitted less than
    /// [`AdmissionRequest::min_repeat_interval`] ago.
    RepeatTooSoon,
    /// The request's position is farther than
    /// [`AdmissionRequest::max_distance`] from the nearest listener.
    TooFar,
}

/// One frame's admission gate: the significance queue's admission controls,
/// pool-agnostic, for hosts gating their own request stream.
///
/// Create one per frame over the matching [`AdmissionState`] and feed it
/// each request in arrival order; [`Self::try_admit`] answers per request
/// and does the bookkeeping (frame counters, repeat-interval recording) for
/// the ones that pass. [`VirtualVoiceQueuePlugin`]'s own admission system is
/// implemented on this same gate, so a host running it against its own
/// requests gets identical semantics, control for control.
pub struct AdmissionGate<'a, M = (), K: Eq + Hash + Clone = AssetId<AudioSample>> {
    state: &'a mut AdmissionState<M, K>,
    frame_budget: Option<usize>,
    now: Duration,
    admitted: usize,
    admitted_per_key: bevy::platform::collections::HashMap<K, usize>,
}

impl<'a, M, K: Eq + Hash + Clone> AdmissionGate<'a, M, K> {
    /// Opens the gate for one frame. `frame_budget` caps admissions across
    /// every sound this frame (the queue passes
    /// [`VirtualVoiceBudget::max_admissions_per_frame`]; `None` admits
    /// everything); `now` is the current [`Time::elapsed`].
    #[must_use]
    pub fn new(
        state: &'a mut AdmissionState<M, K>,
        frame_budget: Option<usize>,
        now: Duration,
    ) -> Self {
        Self {
            state,
            frame_budget,
            now,
            admitted: 0,
            admitted_per_key: Default::default(),
        }
    }

    /// How many requests this gate has admitted so far.
    #[must_use]
    pub fn admitted(&self) -> usize {
        self.admitted
    }

    /// Checks one request against every admission control and, when it
    /// passes, records the admission.
    ///
    /// The environment comes in as closures so it is only computed for
    /// requests that actually set the control needing it: `live_count`
    /// counts the live entries already playing the request's key from
    /// *before* this frame's admissions — the gate adds this frame's own —
    /// where "live" for the queue means virtual or audible, retiring
    /// excluded; `nearest_listener_distance` is the distance from the given
    /// position to the nearest listener, `None` when there is no listener to
    /// measure against (which never culls). The queue measures XY distance
    /// to `SpatialListener2D`s; a host measures however its geometry works.
    pub fn try_admit(
        &mut self,
        request: &AdmissionRequest<K>,
        live_count: impl FnOnce() -> usize,
        nearest_listener_distance: impl FnOnce(Vec2) -> Option<f32>,
    ) -> Result<(), AdmissionRejection> {
        if let Some(cap) = self.frame_budget
            && self.admitted >= cap
        {
            return Err(AdmissionRejection::FrameBudgetSpent);
        }

        let same_this_frame = self
            .admitted_per_key
            .get(&request.key)
            .copied()
            .unwrap_or(0);
        if let Some(cap) = request.max_per_frame
            && same_this_frame >= cap
        {
            return Err(AdmissionRejection::PerFrameCap);
        }
        if let Some(cap) = request.max_concurrent
            && live_count() + same_this_frame >= cap
        {
            return Err(AdmissionRejection::ConcurrentCap);
        }
        if let Some(interval) = request.min_repeat_interval
            && let Some(&last) = self.state.last_admitted.get(&request.key)
            && self.now.saturating_sub(last) < interval
        {
            return Err(AdmissionRejection::RepeatTooSoon);
        }
        if let Some(max_distance) = request.max_distance
            && let Some(position) = request.position
            && nearest_listener_distance(position).is_some_and(|distance| distance > max_distance)
        {
            return Err(AdmissionRejection::TooFar);
        }

        self.admitted += 1;
        *self
            .admitted_per_key
            .entry(request.key.clone())
            .or_insert(0) += 1;
        if let Some(interval) = request.min_repeat_interval {
            self.state.record(request.key.clone(), self.now, interval);
        }
        Ok(())
    }
}

/// One audible voice as a candidate for [`displacement_target`].
///
/// `tier` is an optional *hard* priority layer above significance: a
/// higher-tier newcomer displaces a lower-tier voice regardless of how the
/// significances compare, and a lower-tier newcomer never displaces a
/// higher-tier voice, however loud — the guarantee a raw
/// [`VirtualSound::with_priority`] weight cannot make. The default `()` puts
/// every candidate in the same tier, leaving pure significance with
/// hysteresis.
#[derive(Clone, Copy, Debug)]
pub struct DisplacementCandidate<T = ()> {
    /// The voice's entity, returned when it is the displacement target.
    pub entity: Entity,
    /// The voice's audible significance (volume × priority weight),
    /// sanitized upstream like [`rank_voices`]'s inputs.
    pub significance: f32,
    /// The voice's hard priority tier; higher [`Ord`] wins.
    pub tier: T,
}

impl DisplacementCandidate {
    /// A candidate with no tier: displacement resolves on significance and
    /// margin alone.
    #[must_use]
    pub fn new(entity: Entity, significance: f32) -> Self {
        Self {
            entity,
            significance,
            tier: (),
        }
    }
}

impl<T> DisplacementCandidate<T> {
    /// Places the candidate in a hard priority tier.
    #[must_use]
    pub fn with_tier<U: Ord>(self, tier: U) -> DisplacementCandidate<U> {
        DisplacementCandidate {
            entity: self.entity,
            significance: self.significance,
            tier,
        }
    }
}

/// Answers the displacement question [`rank_voices`] does not: does
/// this *newcomer* outrank the weakest voice it may displace?
///
/// [`rank_voices`] decides which of a whole set should be audible.
/// A host that instead admits one request at a time against a full set of
/// live voices — steal-on-arrival, the way sampler pools usually work —
/// needs the pairwise question. The weakest candidate is the minimum by
/// `tier` first, significance within the tier; the newcomer displaces it
/// when:
///
/// - its tier is strictly higher — the hard gate; symmetrically, a
///   lower-tier newcomer displaces nothing (if it cannot beat the weakest,
///   it cannot beat any), or
/// - the tiers are equal and its significance exceeds the incumbent's times
///   `margin` — the same incumbent-bonus hysteresis as
///   [`VirtualVoiceBudget::displacement_margin`], so pass
///   [`DEFAULT_DISPLACEMENT_MARGIN`] unless you tuned it. Sanitized like the
///   budget's: non-finite becomes `1.0`, and clamped to `>= 1.0`.
///
/// Returns the entity to displace, or `None` when the newcomer should not
/// play. An empty `candidates` returns `None` too — no live voices means a
/// free slot, not a displacement. Ties for weakest resolve to the earliest
/// candidate in input order.
///
/// ```
/// use bevy::prelude::Entity;
/// use msg_seedling::virtual_queue::{
///     DEFAULT_DISPLACEMENT_MARGIN, DisplacementCandidate, displacement_target,
/// };
///
/// #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
/// enum Tier {
///     Low,
///     Critical,
/// }
///
/// let loud_chatter = Entity::PLACEHOLDER;
/// let candidates = [DisplacementCandidate::new(loud_chatter, 8.0).with_tier(Tier::Low)];
///
/// // A quiet critical cue displaces arbitrarily loud low-tier voices...
/// let target = displacement_target(
///     &candidates,
///     &Tier::Critical,
///     0.1,
///     DEFAULT_DISPLACEMENT_MARGIN,
/// );
/// assert_eq!(target, Some(loud_chatter));
///
/// // ...while a same-tier newcomer must beat the incumbent by the margin.
/// let target = displacement_target(&candidates, &Tier::Low, 9.0, DEFAULT_DISPLACEMENT_MARGIN);
/// assert_eq!(target, None);
/// ```
#[must_use]
pub fn displacement_target<T: Ord>(
    candidates: &[DisplacementCandidate<T>],
    newcomer_tier: &T,
    newcomer_significance: f32,
    margin: f32,
) -> Option<Entity> {
    let margin = if margin.is_finite() {
        margin.max(1.0)
    } else {
        1.0
    };
    let weakest = candidates.iter().min_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| a.significance.total_cmp(&b.significance))
    })?;
    match newcomer_tier.cmp(&weakest.tier) {
        Ordering::Greater => Some(weakest.entity),
        Ordering::Equal if newcomer_significance > weakest.significance * margin => {
            Some(weakest.entity)
        }
        _ => None,
    }
}

/// Spawns each [`PlayQueuedAudio`] as an entry of its own, under its parent
/// or at its position. Admission happens in [`admit_virtual_sounds`], the
/// same as for entries the host spawned directly.
fn enqueue_queued_audio<C: AudioCategory>(
    mut commands: Commands,
    entities: &Entities,
    mut messages: MessageReader<PlayQueuedAudio<C>>,
) {
    for msg in messages.read() {
        match (msg.parent, msg.position) {
            (Some(parent), _) => {
                // A spatial sound of a dead emitter must not play at origin.
                if entities.contains(parent) {
                    commands.spawn((msg.sound.clone(), ChildOf(parent), Transform::default()));
                }
            }
            (None, Some(position)) => {
                commands.spawn((
                    msg.sound.clone(),
                    Transform::from_translation(position.as_vec3()),
                ));
            }
            (None, None) => {
                commands.spawn(msg.sound.clone());
            }
        }
    }
}

/// World position an entry plays from, or `None` when it cannot be known yet.
///
/// A child's `GlobalTransform` is only propagated at the end of the frame, so
/// a child is placed from its parent's propagated transform and its own local
/// one. A parent whose `GlobalTransform` appeared since this system last ran
/// has not been propagated either; its position is unknown, which ranking
/// and admission both treat as "could be right next to the listener".
fn world_position(
    transform: Option<&Transform>,
    child_of: Option<&ChildOf>,
    parents: &Query<Ref<GlobalTransform>>,
) -> Option<Vec3> {
    let transform = transform?;
    match child_of {
        Some(child_of) => {
            let parent = parents.get(child_of.parent()).ok()?;
            if parent.is_added() {
                return None;
            }
            Some(parent.transform_point(transform.translation))
        }
        None => Some(transform.translation),
    }
}

/// Admits every entry new this frame: the router picks its lane, and a
/// one-shot passes the [`AdmissionGate`] or is despawned. Loops are always
/// admitted — a loop that does not rank simply waits.
///
/// Higher tiers are admitted first, so under a saturated frame budget a
/// critical cue wins over routine spam; within a tier, creation order.
fn admit_virtual_sounds<C: AudioCategory>(
    mut commands: Commands,
    time: Res<Time>,
    budget: Res<VirtualVoiceBudget<C>>,
    router: Res<QueuePoolRouter<C>>,
    mut admission: ResMut<AdmissionState<C>>,
    mut entries: Query<(
        Entity,
        &mut VirtualSound<C>,
        Option<&Transform>,
        Option<&ChildOf>,
        Has<Retiring>,
    )>,
    parents: Query<Ref<GlobalTransform>>,
    q_listeners: Query<&GlobalTransform, With<SpatialListener2D>>,
) {
    let mut pending: Vec<(Entity, i32, u64)> = Vec::new();
    let mut live: HashMap<AssetId<AudioSample>, usize> = HashMap::default();
    for (entity, sound, _, _, retiring) in &entries {
        match sound.state.lane {
            Some(_) if !retiring => *live.entry(sound.handle.id()).or_default() += 1,
            Some(_) => {}
            None => pending.push((entity, sound.tier, sound.state.sequence)),
        }
    }
    if pending.is_empty() {
        return;
    }
    pending.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));

    let now = time.elapsed();
    let listeners: Vec<Vec2> = q_listeners
        .iter()
        .map(|transform| transform.translation().truncate())
        .collect();
    let mut gate = AdmissionGate::new(&mut admission, budget.max_admissions_per_frame, now);

    for (entity, _, _) in pending {
        let Ok((_, mut sound, transform, child_of, _)) = entries.get_mut(entity) else {
            continue;
        };
        let lane = (router.select)(&sound);
        if lane >= router.lanes.len() {
            error!(
                "virtual queue router chose lane {lane} of {}; dropping the sound",
                router.lanes.len()
            );
            commands.entity(entity).despawn();
            continue;
        }

        if !sound.looping {
            let position = if sound.spatial {
                world_position(transform, child_of, &parents).map(Vec3::truncate)
            } else {
                None
            };
            let request = sound.admission_request(position);
            let admitted = gate.try_admit(
                &request,
                || live.get(&request.key).copied().unwrap_or(0),
                |position| {
                    listeners
                        .iter()
                        .map(|listener| listener.distance(position))
                        .min_by(f32::total_cmp)
                },
            );
            if admitted.is_err() {
                commands.entity(entity).despawn();
                continue;
            }
        }

        if sound.spatial && transform.is_none() {
            commands.entity(entity).insert(Transform::default());
        }
        sound.state.lane = Some(lane);
        sound.state.admitted_at = now;
    }
}

/// One admitted entry's ranking inputs, for [`rank_voices`].
///
/// `K` is the key the per-key cap counts by — the sample's
/// [`AssetId<AudioSample>`] for the built-in queue.
#[derive(Clone, Copy, Debug)]
pub struct VoiceCandidate<K = AssetId<AudioSample>> {
    /// The entry's entity, returned with its decision.
    pub entity: Entity,
    /// Index of the entry's lane in the capacities passed to [`rank_voices`].
    pub lane: usize,
    /// What the per-key cap counts by.
    pub key: K,
    /// Hard priority tier; higher always ranks first.
    pub tier: i32,
    /// The entry's audible significance, with any incumbent bonus already
    /// applied. Zero, negative or NaN never holds a voice.
    pub significance: f32,
    /// Whether the entry holds a voice now.
    pub audible: bool,
    /// Whether the entry may hold a voice at all this frame (e.g. a loop
    /// beyond its maximum distance may not).
    pub eligible: bool,
    /// Cap on voices among entries sharing `key`; `None` never caps.
    pub max_audible: Option<usize>,
    /// Creation order; among equals the newer (higher) entry ranks first.
    pub sequence: u64,
}

/// What [`rank_voices`] decided an entry should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceDecision {
    /// Give this waiting entry a real voice.
    Promote,
    /// Take this entry's real voice away.
    Demote,
    /// No change: already in the right state, or still waiting.
    Hold,
}

/// Pure ranking decision behind [`VirtualVoiceQueuePlugin`], exposed for
/// hosts driving their own pools.
///
/// Entries are ranked by tier, then significance, then newest first. Walking
/// that order, an entry is *selected* when it is eligible, its significance
/// is positive, its lane still has room under `lane_capacity[lane]`, and its
/// key still has room under its own `max_audible`; every selected entry uses
/// up one place of each. A lane index past `lane_capacity` has no room.
///
/// - Selected and waiting: [`VoiceDecision::Promote`].
/// - Not selected and audible: [`VoiceDecision::Demote`].
/// - Otherwise [`VoiceDecision::Hold`].
///
/// A demoted voice and its replacement briefly overlap, so the pool behind a
/// lane needs room for the crossfades in flight on top of the lane's
/// capacity. Decisions are returned in input order, one per candidate.
#[must_use]
pub fn rank_voices<K: Eq + Hash + Clone>(
    candidates: &[VoiceCandidate<K>],
    lane_capacity: &[usize],
) -> Vec<(Entity, VoiceDecision)> {
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|&a, &b| {
        let (a, b) = (&candidates[a], &candidates[b]);
        b.tier
            .cmp(&a.tier)
            .then_with(|| b.significance.total_cmp(&a.significance))
            .then_with(|| b.sequence.cmp(&a.sequence))
    });

    let mut lane_used = vec![0usize; lane_capacity.len()];
    let mut key_used: HashMap<K, usize> = HashMap::default();
    let mut decisions = vec![VoiceDecision::Hold; candidates.len()];
    for &i in &order {
        let candidate = &candidates[i];
        let lane_room = lane_capacity
            .get(candidate.lane)
            .is_some_and(|&capacity| lane_used[candidate.lane] < capacity);
        let key_count = key_used.get(&candidate.key).copied().unwrap_or(0);
        let key_room = candidate.max_audible.is_none_or(|cap| key_count < cap);
        // `>` is false for NaN, so a poisoned weight never holds a voice.
        let selected = candidate.eligible && candidate.significance > 0.0 && lane_room && key_room;
        if selected {
            lane_used[candidate.lane] += 1;
            *key_used.entry(candidate.key.clone()).or_default() += 1;
        }
        decisions[i] = match (selected, candidate.audible) {
            (true, false) => VoiceDecision::Promote,
            (false, true) => VoiceDecision::Demote,
            _ => VoiceDecision::Hold,
        };
    }

    candidates
        .iter()
        .zip(decisions)
        .map(|(candidate, decision)| (candidate.entity, decision))
        .collect()
}

fn rank_virtual_voices<C: AudioCategory>(
    mut commands: Commands,
    budget: Res<VirtualVoiceBudget<C>>,
    router: Res<QueuePoolRouter<C>>,
    config: Res<C::Config>,
    time: Res<Time>,
    default_scale: Option<Res<DefaultSpatialScale>>,
    mut entries: Query<
        (
            Entity,
            &mut VirtualSound<C>,
            Has<Audible>,
            Option<&Transform>,
            Option<&ChildOf>,
            Option<&SpatialScale>,
            Has<UndampedSound>,
        ),
        Without<Retiring>,
    >,
    parents: Query<Ref<GlobalTransform>>,
    q_listeners: Query<&GlobalTransform, With<SpatialListener2D>>,
    q_fields: Query<(&GlobalTransform, &SoundDampingField)>,
) {
    let margin = sanitize_margin(budget.displacement_margin);
    let default_scale = default_scale.map_or(Vec3::ONE, |scale| scale.0);
    let listeners: Vec<Transform> = q_listeners
        .iter()
        .map(GlobalTransform::compute_transform)
        .collect();
    let listener_points: Vec<Vec2> = listeners
        .iter()
        .map(|listener| listener.translation.truncate())
        .collect();
    let fields: Vec<ActiveField> = q_fields
        .iter()
        .filter(|(_, field)| field.radius > 0.0)
        .map(|(transform, field)| {
            let centre = transform.translation().truncate();
            ActiveField::new(centre, field, nearest_listener(&listener_points, centre))
        })
        .collect();

    let mut candidates: Vec<VoiceCandidate> = Vec::new();
    let mut targets: Vec<f32> = Vec::new();
    for (entity, sound, audible, transform, child_of, scale, undamped) in &entries {
        let Some(lane) = sound.state.lane.filter(|&lane| lane < router.lanes.len()) else {
            continue;
        };
        let target_volume = sanitize_weight(sound.category.volume(&config) * sound.base_volume);
        let position = if sound.spatial {
            world_position(transform, child_of, &parents)
        } else {
            None
        };

        let damping = if !fields.is_empty() && sound.category.is_dampable() && !undamped {
            match position {
                Some(position) => SoundDamping::resolve(&fields, position.truncate()).volume,
                None => SoundDamping::resolve_positionless(&fields).volume,
            }
        } else {
            1.0
        };
        // The level the voice's volume node is written to, mapped to the
        // amplitude firewheel actually applies for it.
        let mut significance =
            Volume::Linear(sanitize_weight(target_volume * damping)).amp() * sound.priority;

        let mut eligible = true;
        if let Some(position) = position {
            if let Some(node) = router.lanes[lane].spatial_node() {
                let scale = scale.map_or(default_scale, |scale| scale.0);
                if let Some(distance) = nearest_listener_distance(position, &listeners, scale) {
                    significance *= spatial_gain(node, distance);
                }
            }
            if let Some(max_distance) = sound.max_distance {
                let point = position.truncate();
                let nearest = listener_points
                    .iter()
                    .map(|listener| listener.distance(point))
                    .min_by(f32::total_cmp);
                eligible = nearest.is_none_or(|distance| distance <= max_distance);
            }
        }

        candidates.push(VoiceCandidate {
            entity,
            lane,
            key: sound.handle.id(),
            tier: sound.tier,
            significance: if audible && sound.looping {
                significance * margin
            } else {
                significance
            },
            audible,
            eligible,
            max_audible: sound.max_audible,
            sequence: sound.state.sequence,
        });
        targets.push(target_volume);
    }
    if candidates.is_empty() {
        return;
    }

    let capacity: Vec<usize> = router.lanes.iter().map(QueueLane::max_audible).collect();
    let decisions = rank_voices(&candidates, &capacity);
    let now = time.elapsed();

    for ((entity, decision), (candidate, target_volume)) in
        decisions.into_iter().zip(candidates.iter().zip(targets))
    {
        let Ok((_, mut sound, ..)) = entries.get_mut(entity) else {
            continue;
        };
        match decision {
            VoiceDecision::Promote => {
                // Fresh sounds and every one-shot start at full level; only a
                // loop coming back after waiting fades in.
                let instant =
                    !sound.looping || (!sound.state.promoted && sound.state.admitted_at == now);
                promote(
                    &mut commands,
                    entity,
                    &sound,
                    target_volume,
                    instant,
                    &budget,
                    &router.lanes[candidate.lane],
                );
                sound.state.promoted = true;
            }
            VoiceDecision::Demote => {
                demote(&mut commands, entity, sound.looping, budget.crossfade);
            }
            VoiceDecision::Hold => {
                if !candidate.audible
                    && !sound.looping
                    && now.saturating_sub(sound.state.admitted_at) >= budget.max_wait
                {
                    commands.entity(entity).despawn();
                }
            }
        }
    }
}

/// Gives an entry a real voice in `lane`'s pool.
///
/// `SamplePriority` is inserted at the budget's elevated value plus the
/// entry's tier on every promotion, overwriting the `SamplePriority(0)` a
/// demotion drops the entry to, so a re-promoted loop defends its voice
/// again. The bare `C` component rides along so per-sound category systems
/// (damping, ducking, the config-driven volume rewrites) see the voice, and
/// [`BaseVolume`]/[`BasePitch`] ride along with it so those systems recompute
/// the entry's *own* level and pitch rather than flattening them — the pitch
/// baseline only unless the entry is [`VirtualSound::fixed_pitch`];
/// [`release_voice`] strips all three with the rest of the promotion.
fn promote<C: AudioCategory>(
    commands: &mut Commands,
    entity: Entity,
    sound: &VirtualSound<C>,
    target_volume: f32,
    instant: bool,
    budget: &VirtualVoiceBudget<C>,
    lane: &QueueLane,
) {
    let mut player = SamplePlayer::new(sound.handle.clone());
    if sound.looping {
        player = player.looping();
    }
    let fades_in = !instant && !sound.silent_start;
    let start_volume = if instant && !sound.silent_start {
        target_volume
    } else {
        0.0
    };

    let mut ec = commands.entity(entity);
    if !sound.fixed_pitch {
        ec.insert(BasePitch(sound.pitch));
    }
    ec.insert((
        player,
        sound.category,
        BaseVolume(sound.base_volume),
        // `OnComplete::Remove` keeps the entity when seedling ends the voice
        // (steal, queue expiry, one-shot completion) so `reclaim_lost_voices`
        // can apply the queue's own policy.
        PlaybackSettings {
            on_complete: OnComplete::Remove,
            ..Default::default()
        }
        // Firewheel's playback speed is `f32` in some releases and `f64` in
        // others; the `f32` baseline widens into either losslessly.
        .with_speed(sound.pitch.into()),
        SamplePriority(budget.sample_priority.saturating_add(sound.tier)),
        sample_effects![VolumeNode::from_linear(start_volume)],
        Audible,
    ));
    if fades_in {
        ec.insert(FadeInAudio::new(budget.crossfade, target_volume));
    }
    ec.insert_if_new(Name::new(format!("{:?}", sound.category)));
    (lane.route)(&mut ec);
}

/// Starts a demotion fade: looping entries keep their entity and return to
/// virtual once the fade completes; one-shots despawn with it. The voice
/// drops back to default `SamplePriority`, so pool pressure steals the
/// nearly-silent fader before any live promoted voice.
fn demote(commands: &mut Commands, entity: Entity, looping: bool, crossfade: Duration) {
    let fade = if looping {
        FadeOutAudio::new(crossfade).keep_entity()
    } else {
        FadeOutAudio::new(crossfade)
    };
    commands
        .entity(entity)
        .remove::<(Audible, FadeInAudio)>()
        .insert((Retiring, SamplePriority(0), fade));
}

/// The release hook of the lane an entry was promoted into.
fn lane_release<'a, C: AudioCategory>(
    router: &'a QueuePoolRouter<C>,
    sound: &VirtualSound<C>,
) -> Option<&'a LabelHook> {
    sound
        .state
        .lane
        .and_then(|lane| router.lanes.get(lane))
        .map(|lane| &lane.release)
}

/// Returns a demoted looping entry to the virtual state once its fade-out
/// completes ([`FadeOutAudio`] removes itself, leaving `Retiring` behind).
fn finish_demotions<C: AudioCategory>(
    mut commands: Commands,
    router: Res<QueuePoolRouter<C>>,
    finished: Query<
        (Entity, &VirtualSound<C>, Has<Sampler>),
        (With<Retiring>, Without<FadeOutAudio>),
    >,
) {
    for (entity, sound, has_sampler) in &finished {
        release_voice::<C>(
            &mut commands,
            entity,
            has_sampler,
            lane_release(&router, sound),
        );
    }
}

/// Applies queue policy to promoted entries whose seedling voice ended
/// (`OnComplete::Remove` stripped the `SamplePlayer`): looping entries
/// return to virtual for re-promotion, finished or dead one-shots despawn.
fn reclaim_lost_voices<C: AudioCategory>(
    mut commands: Commands,
    router: Res<QueuePoolRouter<C>>,
    lost: Query<
        (Entity, &VirtualSound<C>, Has<Sampler>),
        (With<Audible>, Without<SamplePlayer>, Without<Retiring>),
    >,
) {
    for (entity, sound, has_sampler) in &lost {
        if sound.looping {
            release_voice::<C>(
                &mut commands,
                entity,
                has_sampler,
                lane_release(&router, sound),
            );
        } else {
            commands.entity(entity).despawn();
        }
    }
}

/// Strips everything a promotion added — the bare `C` component, the
/// per-sound baselines, and (through the lane's release hook) the pool
/// label — leaving a bare [`VirtualSound`] entry eligible for re-promotion.
/// The entry's authored volume and pitch survive in the [`VirtualSound`], so
/// the next promotion restores them.
fn release_voice<C: AudioCategory>(
    commands: &mut Commands,
    entity: Entity,
    has_sampler: bool,
    release: Option<&LabelHook>,
) {
    // A live sampler needs seedling's completion observer (when
    // `SeedlingPlugin` is present) to release it and strip its private
    // bookkeeping. On the reclaim path seedling already completed the voice
    // and removed `Sampler`, so skipping the trigger there avoids a second
    // `PlaybackCompletion` for the same voice; the explicit removes below
    // cover everything else either way.
    if has_sampler {
        commands.trigger(PlaybackCompletion {
            entity,
            reason: CompletionReason::PlaybackInterrupted,
        });
    }
    let mut ec = commands.entity(entity);
    if let Some(release) = release {
        release(&mut ec);
    }
    ec.despawn_related::<SampleEffects>()
        .remove_with_requires::<SamplePlayer>()
        .remove::<(
            C,
            BaseVolume,
            BasePitch,
            Sampler,
            QueuedSample,
            AudioEvents,
            PoolLabelContainer,
            Audible,
            Retiring,
            FadeInAudio,
            FadeOutAudio,
        )>();
}

fn handle_stop_queued_audio<C: AudioCategory>(
    mut commands: Commands,
    budget: Res<VirtualVoiceBudget<C>>,
    mut messages: MessageReader<StopQueuedAudio<C>>,
    entries: Query<(Entity, &VirtualSound<C>, Has<Audible>, Has<Retiring>)>,
    mut fades: Query<&mut FadeOutAudio>,
) {
    for msg in messages.read() {
        for (entity, sound, audible, retiring) in &entries {
            if let Some(category) = msg.category
                && sound.category != category
            {
                continue;
            }
            if let Some(handle) = &msg.handle
                && sound.handle != *handle
            {
                continue;
            }
            if audible {
                // Fade out through the demotion path, but always despawn.
                demote(&mut commands, entity, false, budget.crossfade);
            } else if retiring {
                match fades.get_mut(entity) {
                    // A finished fade completes (removal or despawn) before
                    // this system reads it again — mutating it would be
                    // lost, so despawn outright.
                    Ok(fade) if fade.is_complete() => commands.entity(entity).despawn(),
                    Ok(mut fade) => fade.despawn_on_complete = true,
                    Err(_) => commands.entity(entity).despawn(),
                }
            } else {
                commands.entity(entity).despawn();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use msg_testing::{AppTesting, physics_app};

    use super::*;
    use crate::traits::AudioConfig;

    #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
    #[reflect(Component)]
    enum TestSound {
        #[default]
        Sfx,
        Ambience,
    }

    #[derive(Resource, Clone, Reflect)]
    #[reflect(Resource)]
    struct TestConfig {
        sfx: f32,
        ambience: f32,
    }

    impl Default for TestConfig {
        fn default() -> Self {
            Self {
                sfx: 1.0,
                ambience: 1.0,
            }
        }
    }

    impl AudioConfig for TestConfig {
        fn master_volume(&self) -> f32 {
            1.0
        }
    }

    impl AudioCategory for TestSound {
        type Config = TestConfig;
        fn volume(&self, config: &Self::Config) -> f32 {
            match self {
                TestSound::Sfx => config.sfx,
                TestSound::Ambience => config.ambience,
            }
        }
    }

    /// The default two-lane router with `voices` per lane.
    fn router(voices: usize) -> QueuePoolRouter<TestSound> {
        QueuePoolRouter::new(
            vec![
                QueueLane::new(SpatialPool, voices).with_spatial(SpatialBasicNode::default()),
                QueueLane::new(DefaultPool, voices),
            ],
            |sound| usize::from(!sound.is_spatial()),
        )
    }

    fn queue_app(voices: usize, budget: VirtualVoiceBudget<TestSound>) -> App {
        let mut app = physics_app();
        app.add_plugins(
            VirtualVoiceQueuePlugin::new()
                .with_budget(budget)
                .with_pool_router(router(voices)),
        );
        app
    }

    fn handle(id: u128) -> Handle<AudioSample> {
        Handle::Uuid(
            bevy::asset::uuid::Uuid::from_u128(id),
            core::marker::PhantomData,
        )
    }

    fn sound(id: u128) -> VirtualSound<TestSound> {
        VirtualSound::new(handle(id), TestSound::Sfx)
    }

    fn play(app: &mut App, sound: VirtualSound<TestSound>) {
        app.world_mut().write_message(PlayQueuedAudio::new(sound));
    }

    fn spawn(app: &mut App, bundle: impl Bundle) -> Entity {
        app.world_mut().spawn(bundle).id()
    }

    fn audible(app: &App, entity: Entity) -> bool {
        app.world().get::<Audible>(entity).is_some()
    }

    fn entry_count(app: &mut App) -> usize {
        let world = app.world_mut();
        world
            .query::<&VirtualSound<TestSound>>()
            .iter(world)
            .count()
    }

    fn audible_handles(app: &mut App) -> Vec<Handle<AudioSample>> {
        let world = app.world_mut();
        let mut handles: Vec<Handle<AudioSample>> = world
            .query_filtered::<&VirtualSound<TestSound>, With<Audible>>()
            .iter(world)
            .map(|sound| sound.handle.clone())
            .collect();
        handles.sort_by_key(Handle::id);
        handles
    }

    fn listener_at(app: &mut App, position: Vec2) -> Entity {
        let transform = Transform::from_translation(position.extend(0.0));
        spawn(
            app,
            (
                SpatialListener2D,
                transform,
                GlobalTransform::from(transform),
            ),
        )
    }

    // ==================== Pure ranking ====================

    fn eid(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("test id is a valid entity index")
    }

    fn candidate(id: u32, significance: f32, audible: bool) -> VoiceCandidate<u32> {
        VoiceCandidate {
            entity: eid(id),
            lane: 0,
            key: id,
            tier: 0,
            significance,
            audible,
            eligible: true,
            max_audible: None,
            sequence: u64::from(id),
        }
    }

    fn decide(
        candidates: &[VoiceCandidate<u32>],
        capacity: &[usize],
    ) -> HashMap<Entity, VoiceDecision> {
        rank_voices(candidates, capacity).into_iter().collect()
    }

    #[test]
    fn the_most_significant_fill_the_lane() {
        let decisions = decide(
            &[
                candidate(0, 1.0, false),
                candidate(1, 0.5, false),
                candidate(2, 0.8, false),
            ],
            &[2],
        );
        assert_eq!(decisions[&eid(0)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(2)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Hold);
    }

    #[test]
    fn a_more_significant_newcomer_demotes_the_weakest_audible() {
        let decisions = decide(
            &[
                candidate(0, 0.9, true),
                candidate(1, 0.2, true),
                candidate(2, 1.0, false),
            ],
            &[2],
        );
        assert_eq!(decisions[&eid(2)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(0)], VoiceDecision::Hold);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Demote);
    }

    #[test]
    fn a_less_significant_newcomer_does_not_disturb_playing_voices() {
        let decisions = decide(
            &[
                candidate(0, 0.9, true),
                candidate(1, 0.7, true),
                candidate(2, 0.1, false),
            ],
            &[2],
        );
        assert_eq!(decisions[&eid(0)], VoiceDecision::Hold);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Hold);
        assert_eq!(decisions[&eid(2)], VoiceDecision::Hold);
    }

    #[test]
    fn ties_go_to_the_newest() {
        let decisions = decide(
            &[
                candidate(0, 0.5, true),
                candidate(1, 0.5, false),
                candidate(2, 0.5, false),
            ],
            &[2],
        );
        assert_eq!(decisions[&eid(2)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(0)], VoiceDecision::Demote);
    }

    #[test]
    fn a_higher_tier_outranks_any_significance() {
        let mut quiet_critical = candidate(0, 0.01, false);
        quiet_critical.tier = 3;
        let decisions = decide(&[candidate(1, 100.0, true), quiet_critical], &[1]);
        assert_eq!(decisions[&eid(0)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Demote);
    }

    #[test]
    fn a_key_cap_keeps_the_most_significant_copies_only() {
        let copy = |id, significance, audible| VoiceCandidate {
            key: 7,
            max_audible: Some(2),
            ..candidate(id, significance, audible)
        };
        let decisions = decide(
            &[
                copy(0, 0.3, true),
                copy(1, 0.9, false),
                copy(2, 0.6, true),
                copy(3, 0.1, false),
                candidate(4, 0.05, false),
            ],
            &[10],
        );
        assert_eq!(decisions[&eid(1)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(2)], VoiceDecision::Hold);
        assert_eq!(decisions[&eid(0)], VoiceDecision::Demote, "quietest copy");
        assert_eq!(decisions[&eid(3)], VoiceDecision::Hold);
        assert_eq!(
            decisions[&eid(4)],
            VoiceDecision::Promote,
            "another sample is not counted against the cap"
        );
    }

    #[test]
    fn lanes_fill_independently() {
        let in_lane = |id, significance, lane| VoiceCandidate {
            lane,
            ..candidate(id, significance, false)
        };
        let decisions = decide(
            &[in_lane(0, 1.0, 0), in_lane(1, 0.9, 0), in_lane(2, 0.1, 1)],
            &[1, 1],
        );
        assert_eq!(decisions[&eid(0)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Hold);
        assert_eq!(decisions[&eid(2)], VoiceDecision::Promote);
    }

    #[test]
    fn a_lane_past_the_capacities_has_no_room() {
        let stray = VoiceCandidate {
            lane: 5,
            ..candidate(0, 1.0, true)
        };
        assert_eq!(decide(&[stray], &[4])[&eid(0)], VoiceDecision::Demote);
    }

    #[test]
    fn inaudible_and_ineligible_entries_never_hold_a_voice() {
        let ineligible = VoiceCandidate {
            eligible: false,
            ..candidate(3, 1.0, true)
        };
        let decisions = decide(
            &[
                candidate(0, 0.0, false),
                candidate(1, f32::NAN, true),
                candidate(2, -1.0, false),
                ineligible,
            ],
            &[8],
        );
        assert_eq!(decisions[&eid(0)], VoiceDecision::Hold);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Demote);
        assert_eq!(decisions[&eid(2)], VoiceDecision::Hold);
        assert_eq!(decisions[&eid(3)], VoiceDecision::Demote);
    }

    #[test]
    fn zero_capacity_holds_or_demotes_everything() {
        let decisions = decide(&[candidate(0, 1.0, true), candidate(1, 0.5, false)], &[0]);
        assert_eq!(decisions[&eid(0)], VoiceDecision::Demote);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Hold);
    }

    #[test]
    fn decisions_are_returned_in_input_order() {
        let decisions = rank_voices(
            &[
                candidate(7, 0.1, false),
                candidate(3, 0.9, false),
                candidate(5, 0.5, false),
            ],
            &[1],
        );
        let order: Vec<Entity> = decisions.iter().map(|(entity, _)| *entity).collect();
        assert_eq!(order, vec![eid(7), eid(3), eid(5)]);
    }

    // ==================== Builders ====================

    #[test]
    fn budget_builder_sets_all_fields() {
        let budget = VirtualVoiceBudget::<TestSound>::new()
            .with_crossfade(Duration::from_millis(30))
            .with_max_wait(Duration::from_millis(200))
            .with_sample_priority(5)
            .with_max_admissions_per_frame(9)
            .with_displacement_margin(2.0);

        assert_eq!(budget.crossfade, Duration::from_millis(30));
        assert_eq!(budget.max_wait, Duration::from_millis(200));
        assert_eq!(budget.sample_priority, 5);
        assert_eq!(budget.max_admissions_per_frame, Some(9));
        assert_eq!(budget.displacement_margin, 2.0);
    }

    #[test]
    fn budget_builder_sanitizes_the_displacement_margin() {
        let below_one = VirtualVoiceBudget::<TestSound>::new().with_displacement_margin(0.5);
        assert_eq!(below_one.displacement_margin, 1.0);

        let non_finite = VirtualVoiceBudget::<TestSound>::new().with_displacement_margin(f32::NAN);
        assert_eq!(non_finite.displacement_margin, 1.0);
    }

    #[test]
    fn virtual_sound_builder_sanitizes_hostile_values() {
        let hostile = sound(1)
            .with_volume(f32::NAN)
            .with_priority(-3.0)
            .with_pitch(0.0)
            .with_max_distance(f32::NAN);
        assert_eq!(hostile.base_volume(), 0.0);
        assert_eq!(hostile.priority(), 0.0);
        assert_eq!(hostile.pitch(), 1.0);
        assert_eq!(
            hostile.max_distance, None,
            "a non-finite distance never culls"
        );
        assert_eq!(sound(1).with_max_distance(-5.0).max_distance, Some(0.0));
    }

    #[test]
    fn newer_sounds_carry_a_higher_sequence() {
        let older = sound(1);
        let newer = sound(1);
        assert!(newer.state.sequence > older.state.sequence);
    }

    #[test]
    fn positioning_a_request_makes_it_spatial() {
        assert!(!PlayQueuedAudio::new(sound(1)).sound.is_spatial());
        assert!(
            PlayQueuedAudio::new(sound(1))
                .at(Vec2::ONE)
                .sound
                .is_spatial()
        );
        assert!(
            PlayQueuedAudio::new(sound(1))
                .with_parent(eid(3))
                .sound
                .is_spatial()
        );
    }

    #[test]
    fn admission_request_mirrors_the_entry_controls() {
        let request = sound(1)
            .with_max_concurrent(4)
            .with_max_per_frame(2)
            .with_min_repeat_interval(Duration::from_millis(40))
            .with_max_distance(1200.0)
            .admission_request(Some(Vec2::new(3.0, 4.0)));

        assert_eq!(request.key, handle(1).id());
        assert_eq!(request.position, Some(Vec2::new(3.0, 4.0)));
        assert_eq!(request.max_concurrent, Some(4));
        assert_eq!(request.max_per_frame, Some(2));
        assert_eq!(request.min_repeat_interval, Some(Duration::from_millis(40)));
        assert_eq!(request.max_distance, Some(1200.0));
    }

    // ==================== Promotion ====================

    fn the_voice(app: &mut App) -> Entity {
        let world = app.world_mut();
        world
            .query_filtered::<Entity, With<Audible>>()
            .single(world)
            .expect("exactly one promoted voice")
    }

    fn voice_volume(app: &App, voice: Entity) -> f32 {
        let effect = app.world().get::<SampleEffects>(voice).expect("effects")[0];
        app.world()
            .get::<VolumeNode>(effect)
            .expect("volume node")
            .volume
            .linear()
    }

    #[test]
    fn promotion_carries_tiered_sample_priority_pitch_and_name() {
        let mut app = queue_app(1, VirtualVoiceBudget::new().with_sample_priority(3));
        play(&mut app, sound(1).looping().with_tier(2).with_pitch(1.3));
        app.update_n(1);

        let voice = the_voice(&mut app);
        let world = app.world();
        assert_eq!(world.get::<SamplePriority>(voice).unwrap().0, 5);
        let settings = world.get::<PlaybackSettings>(voice).unwrap();
        assert!(matches!(settings.on_complete, OnComplete::Remove));
        assert!((settings.speed - f64::from(1.3_f32)).abs() < 1e-9);
        assert_eq!(world.get::<BasePitch>(voice), Some(&BasePitch(1.3)));
        assert!(world.get::<Name>(voice).is_some());
    }

    #[test]
    fn a_fixed_pitch_voice_carries_no_pitch_baseline_for_damping_to_bend() {
        let mut app = queue_app(1, VirtualVoiceBudget::new());
        play(&mut app, sound(1).looping().with_pitch(1.5).fixed_pitch());
        app.update_n(1);

        let voice = the_voice(&mut app);
        assert!(app.world().get::<BasePitch>(voice).is_none());
        let speed = app.world().get::<PlaybackSettings>(voice).unwrap().speed;
        assert!(
            (speed - 1.5).abs() < 1e-6,
            "it still plays at its own pitch"
        );
    }

    #[test]
    fn fresh_sounds_start_at_full_level_without_a_fade() {
        for looping in [false, true] {
            let mut app = queue_app(1, VirtualVoiceBudget::new());
            let fresh = sound(1).with_volume(0.5);
            play(&mut app, if looping { fresh.looping() } else { fresh });
            app.update_n(1);

            let voice = the_voice(&mut app);
            assert!(
                app.world().get::<FadeInAudio>(voice).is_none(),
                "looping={looping}"
            );
            assert!(
                (voice_volume(&app, voice) - 0.5).abs() < 1e-6,
                "looping={looping}"
            );
        }
    }

    #[test]
    fn a_waiting_loop_fades_in_when_it_is_promoted() {
        let mut app = queue_app(1, VirtualVoiceBudget::new().with_crossfade(Duration::ZERO));
        play(&mut app, sound(1).looping());
        let waiting = spawn(&mut app, sound(2).looping().with_volume(0.5));
        app.update_n(1);
        assert!(!audible(&app, waiting), "the quieter loop waits");

        app.world_mut()
            .write_message(StopQueuedAudio::<TestSound>::all().with_handle(handle(1)));
        app.update_n(60);

        assert!(audible(&app, waiting));
        let fade = app
            .world()
            .get::<FadeInAudio>(waiting)
            .expect("a loop coming back fades in");
        assert!((fade.target_volume - 0.5).abs() < 1e-6);
        assert_eq!(voice_volume(&app, waiting), 0.0);
    }

    #[test]
    fn a_silent_start_leaves_the_level_to_the_host() {
        let mut app = queue_app(1, VirtualVoiceBudget::new());
        play(&mut app, sound(1).looping().silent_start());
        app.update_n(1);

        let voice = the_voice(&mut app);
        assert!(app.world().get::<FadeInAudio>(voice).is_none());
        assert_eq!(voice_volume(&app, voice), 0.0);
    }

    #[test]
    fn a_host_spawned_entry_keeps_its_own_components_across_demotion() {
        #[derive(Component)]
        struct Marker;

        let mut app = queue_app(1, VirtualVoiceBudget::new().with_crossfade(Duration::ZERO));
        let entry = spawn(
            &mut app,
            (
                sound(1).looping().with_volume(0.5),
                Marker,
                Name::new("mine"),
            ),
        );
        app.update_n(1);
        assert!(audible(&app, entry));
        assert_eq!(app.world().get::<Name>(entry).unwrap().as_str(), "mine");

        play(&mut app, sound(2).looping());
        app.update_n(60);

        let world = app.world();
        assert!(world.get::<VirtualSound<TestSound>>(entry).is_some());
        assert!(world.get::<Audible>(entry).is_none());
        assert!(world.get::<Marker>(entry).is_some());
        assert_eq!(world.get::<Name>(entry).unwrap().as_str(), "mine");
    }

    // ==================== Demotion and lifecycle ====================

    #[test]
    fn demoted_looping_entry_returns_to_virtual() {
        let mut app = queue_app(1, VirtualVoiceBudget::new().with_crossfade(Duration::ZERO));
        let quiet = spawn(&mut app, sound(1).looping().with_volume(0.5));
        app.update_n(1);
        assert!(audible(&app, quiet));

        // A louder loop displaces it.
        play(&mut app, sound(2).looping());
        app.update_n(1);
        assert!(!audible(&app, quiet));
        assert!(app.world().get::<Retiring>(quiet).is_some());
        let fade = app.world().get::<FadeOutAudio>(quiet).expect("fading out");
        assert!(!fade.despawn_on_complete);
        // The retiring fader drops to default priority so pool pressure
        // steals it before any live promoted voice.
        assert_eq!(app.world().get::<SamplePriority>(quiet).unwrap().0, 0);

        // Effects never resolve in tests, so completion waits out the
        // unresolved-effects grace; step well past it.
        app.update_n(60);
        let world = app.world();
        assert!(world.get_entity(quiet).is_ok(), "loop entry survives");
        assert!(world.get::<Retiring>(quiet).is_none());
        assert!(world.get::<Audible>(quiet).is_none());
        assert!(world.get::<SamplePlayer>(quiet).is_none());
        assert!(world.get::<SpatialPool>(quiet).is_none());
        assert!(world.get::<DefaultPool>(quiet).is_none());
    }

    #[test]
    fn demoted_one_shot_despawns_after_its_fade() {
        let mut app = queue_app(1, VirtualVoiceBudget::new().with_crossfade(Duration::ZERO));
        let one_shot = spawn(&mut app, sound(1).with_volume(0.5));
        app.update_n(1);
        assert!(audible(&app, one_shot));

        play(&mut app, sound(2));
        app.update_n(60);
        assert!(
            app.world().get_entity(one_shot).is_err(),
            "one-shot dropped"
        );
    }

    #[test]
    fn a_new_one_shot_takes_the_oldest_copy_s_voice_under_a_cap() {
        let mut app = queue_app(8, VirtualVoiceBudget::new());
        let oldest = spawn(&mut app, sound(1).with_max_audible(1));
        app.update_n(1);
        assert!(audible(&app, oldest));

        let newest = spawn(&mut app, sound(1).with_max_audible(1));
        app.update_n(1);
        assert!(audible(&app, newest), "the new hit plays");
        assert!(
            app.world().get::<Retiring>(oldest).is_some(),
            "the old tail goes"
        );
    }

    #[test]
    fn released_loop_is_repromoted_when_a_slot_frees_up() {
        let mut app = queue_app(1, VirtualVoiceBudget::new().with_crossfade(Duration::ZERO));
        let quiet = spawn(&mut app, sound(1).looping().with_volume(0.5));
        play(&mut app, sound(2).looping());
        app.update_n(1);
        assert!(!audible(&app, quiet), "quieter loop waits as virtual");

        // Stop the loud loop; once its fade-out completes, the waiting loop
        // is promoted.
        app.world_mut()
            .write_message(StopQueuedAudio::<TestSound>::all().with_handle(handle(2)));
        app.update_n(60);
        assert!(audible(&app, quiet));
    }

    #[test]
    fn reclaim_applies_policy_to_voices_seedling_ended() {
        // No voices at all, so a reclaimed loop is not immediately re-promoted.
        let mut app = queue_app(0, VirtualVoiceBudget::new());
        // Simulate `OnComplete::Remove` aftermath: Audible without a
        // `SamplePlayer`, on admitted entries.
        let admitted = |sound: VirtualSound<TestSound>| {
            let mut sound = sound;
            sound.state.lane = Some(1);
            sound
        };
        let looping = spawn(&mut app, (admitted(sound(1).looping()), Audible));
        let one_shot = spawn(&mut app, (admitted(sound(2)), Audible));

        app.update_n(1);
        assert!(
            app.world().get_entity(one_shot).is_err(),
            "one-shot dropped"
        );
        assert!(app.world().get_entity(looping).is_ok(), "loop kept");
        assert!(!audible(&app, looping));
    }

    #[test]
    fn config_volume_changes_rerank_waiting_and_audible_entries() {
        let mut app = queue_app(1, VirtualVoiceBudget::new().with_crossfade(Duration::ZERO));
        let sfx = spawn(&mut app, sound(1).looping());
        let ambience = spawn(
            &mut app,
            VirtualSound::new(handle(2), TestSound::Ambience)
                .looping()
                .with_volume(0.5),
        );
        app.update_n(1);
        assert!(audible(&app, sfx));

        // Boost ambience volume: the waiting entry now outranks the SFX one.
        app.world_mut().resource_mut::<TestConfig>().ambience = 10.0;
        app.update_n(1);
        assert!(audible(&app, ambience));
        assert!(app.world().get::<Retiring>(sfx).is_some());
    }

    #[test]
    fn nan_config_volume_is_sanitized_before_ranking() {
        let mut app = queue_app(1, VirtualVoiceBudget::new());
        app.world_mut().resource_mut::<TestConfig>().sfx = f32::NAN;
        spawn(&mut app, sound(1).looping());
        let ambience = spawn(
            &mut app,
            VirtualSound::new(handle(2), TestSound::Ambience)
                .looping()
                .with_volume(0.5),
        );
        app.update_n(1);

        // NaN significance would outrank everything under `total_cmp`; the
        // sanitized recompute ranks the NaN-config category at zero instead.
        assert_eq!(audible_handles(&mut app), vec![handle(2)]);
        assert!(audible(&app, ambience));
    }

    #[test]
    fn loops_wait_while_one_shots_give_up_after_max_wait() {
        let mut app = queue_app(
            0,
            VirtualVoiceBudget::new().with_max_wait(Duration::from_millis(50)),
        );
        play(&mut app, sound(1).looping());
        play(&mut app, sound(2));
        app.update_n(10);

        let world = app.world_mut();
        let remaining: Vec<bool> = world
            .query::<&VirtualSound<TestSound>>()
            .iter(world)
            .map(VirtualSound::is_looping)
            .collect();
        assert_eq!(remaining, vec![true], "one-shot dropped, loop waits on");
    }

    #[test]
    fn without_a_wait_a_one_shot_that_does_not_rank_is_dropped_at_once() {
        let mut app = queue_app(1, VirtualVoiceBudget::new().with_max_wait(Duration::ZERO));
        play(&mut app, sound(1).looping());
        play(&mut app, sound(2).with_volume(0.1));
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 1);
    }

    #[test]
    fn entries_with_a_dead_parent_are_dropped() {
        let mut app = queue_app(1, VirtualVoiceBudget::new());
        let parent = spawn(&mut app, ());
        app.world_mut()
            .write_message(PlayQueuedAudio::new(sound(1).looping()).with_parent(parent));
        app.world_mut().despawn(parent);
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 0);
    }

    #[test]
    fn stop_queued_audio_despawns_virtual_and_fades_audible() {
        let mut app = queue_app(1, VirtualVoiceBudget::new());
        play(&mut app, sound(1).looping());
        play(&mut app, sound(2).looping().with_volume(0.5));
        app.update_n(1);

        app.world_mut()
            .write_message(StopQueuedAudio::category(TestSound::Sfx));
        app.update_n(1);

        let world = app.world_mut();
        let mut entries = world.query::<(&VirtualSound<TestSound>, &FadeOutAudio)>();
        let (_, fade) = entries
            .single(world)
            .expect("only the audible entry remains");
        assert!(fade.despawn_on_complete);

        app.update_n(60);
        assert_eq!(entry_count(&mut app), 0);
    }

    #[test]
    fn stop_queued_audio_handle_filter_stops_only_matching_entries() {
        let mut app = queue_app(0, VirtualVoiceBudget::new());
        play(&mut app, sound(1).looping());
        play(&mut app, sound(2).looping());
        app.update_n(1);

        app.world_mut()
            .write_message(StopQueuedAudio::<TestSound>::all().with_handle(handle(1)));
        app.update_n(1);

        let world = app.world_mut();
        let remaining: Vec<Handle<AudioSample>> = world
            .query::<&VirtualSound<TestSound>>()
            .iter(world)
            .map(|sound| sound.handle.clone())
            .collect();
        assert_eq!(remaining, vec![handle(2)]);
    }

    #[test]
    fn stop_around_fade_completion_always_despawns() {
        // A stop arriving on the exact frame a keep-entity demotion fade
        // completes must not be lost to the fade's own self-removal —
        // sweep stop delivery across the completion window to cover it.
        for stop_after in (0..=40).step_by(2) {
            let mut app = queue_app(1, VirtualVoiceBudget::new().with_crossfade(Duration::ZERO));
            play(&mut app, sound(1).looping());
            app.update_n(1);
            // Loud enough to beat the displacement margin and demote the
            // incumbent into its keep-entity retiring fade.
            play(&mut app, sound(2).looping().with_volume(2.0));
            app.update_n(1);

            app.update_n(stop_after);
            app.world_mut()
                .write_message(StopQueuedAudio::<TestSound>::all().with_handle(handle(1)));
            app.update_n(60);

            let world = app.world_mut();
            let survivors: Vec<Handle<AudioSample>> = world
                .query::<&VirtualSound<TestSound>>()
                .iter(world)
                .map(|sound| sound.handle.clone())
                .collect();
            assert_eq!(survivors, vec![handle(2)], "stop_after={stop_after}");
        }
    }

    // ==================== Ranking in the plugin ====================

    #[test]
    fn a_loop_newcomer_inside_the_displacement_margin_does_not_displace() {
        let mut app = queue_app(1, VirtualVoiceBudget::new());
        let incumbent = spawn(&mut app, sound(1).looping());
        app.update_n(1);

        // 10% louder: inside the default 1.25 margin, so no churn.
        play(&mut app, sound(2).looping().with_volume(1.1));
        app.update_n(2);
        assert!(audible(&app, incumbent));
        assert!(app.world().get::<Retiring>(incumbent).is_none());
    }

    #[test]
    fn a_loop_newcomer_beyond_the_displacement_margin_displaces() {
        let mut app = queue_app(1, VirtualVoiceBudget::new());
        let incumbent = spawn(&mut app, sound(1).looping());
        app.update_n(1);

        // 30% louder: beats the default 1.25 margin.
        play(&mut app, sound(2).looping().with_volume(1.3));
        app.update_n(1);
        assert!(!audible(&app, incumbent));
        assert!(app.world().get::<Retiring>(incumbent).is_some());
    }

    #[test]
    fn a_higher_tier_takes_the_voice_however_quiet() {
        let mut app = queue_app(1, VirtualVoiceBudget::new());
        let loud = spawn(&mut app, sound(1).looping().with_volume(2.0));
        app.update_n(1);
        assert!(audible(&app, loud));

        let critical = spawn(&mut app, sound(2).looping().with_volume(0.05).with_tier(1));
        app.update_n(1);
        assert!(audible(&app, critical));
        assert!(app.world().get::<Retiring>(loud).is_some());
    }

    #[test]
    fn the_nearer_copy_holds_the_voice_and_moving_hands_it_over() {
        let mut app = queue_app(8, VirtualVoiceBudget::new());
        let listener = listener_at(&mut app, Vec2::ZERO);
        let drone = |x: f32| {
            (
                sound(1).looping().spatial().with_max_audible(1),
                Transform::from_xyz(x, 0.0, 0.0),
            )
        };
        let far = spawn(&mut app, drone(1000.0));
        let near = spawn(&mut app, drone(50.0));
        app.update_n(1);
        assert!(audible(&app, near));
        assert!(!audible(&app, far));

        let moved = Transform::from_xyz(990.0, 0.0, 0.0);
        app.world_mut()
            .entity_mut(listener)
            .insert((moved, GlobalTransform::from(moved)));
        app.update_n(1);
        assert!(audible(&app, far), "the drone the listener flew to");
        assert!(app.world().get::<Retiring>(near).is_some());
    }

    #[test]
    fn distance_is_measured_at_the_spatial_scale() {
        let mut app = queue_app(1, VirtualVoiceBudget::new());
        app.insert_resource(DefaultSpatialScale(Vec3::splat(0.01)));
        listener_at(&mut app, Vec2::ZERO);
        // 400 world units are 4 audio units: inside the default 5-unit
        // reference distance at this scale, so no attenuation at all, and the
        // louder near-ish sound beats a quieter one at the listener.
        let scaled = spawn(
            &mut app,
            (
                sound(1).looping().spatial().with_volume(0.9),
                Transform::from_xyz(400.0, 0.0, 0.0),
            ),
        );
        spawn(
            &mut app,
            (
                sound(2).looping().spatial().with_volume(0.8),
                Transform::default(),
            ),
        );
        app.update_n(1);
        assert!(audible(&app, scaled));
    }

    #[test]
    fn a_lane_without_a_distance_model_ignores_distance() {
        let mut app = physics_app();
        app.add_plugins(
            VirtualVoiceQueuePlugin::<TestSound>::new().with_pool_router(QueuePoolRouter::new(
                vec![QueueLane::new(SpatialPool, 1)],
                |_| 0,
            )),
        );
        listener_at(&mut app, Vec2::ZERO);
        let far_loud = spawn(
            &mut app,
            (
                sound(1).looping().spatial(),
                Transform::from_xyz(5000.0, 0.0, 0.0),
            ),
        );
        spawn(
            &mut app,
            (
                sound(2).looping().spatial().with_volume(0.5),
                Transform::default(),
            ),
        );
        app.update_n(1);
        assert!(audible(&app, far_loud));
    }

    #[test]
    fn damping_counts_toward_significance() {
        let mut app = queue_app(1, VirtualVoiceBudget::new());
        spawn(
            &mut app,
            (
                SoundDampingField {
                    radius: 100.0,
                    volume: 0.1,
                    ..Default::default()
                },
                GlobalTransform::default(),
            ),
        );
        let at_centre = |id| (sound(id).looping().spatial(), Transform::default());
        let muffled = spawn(&mut app, at_centre(1));
        let outside = spawn(
            &mut app,
            (
                sound(2).looping().spatial().with_volume(0.5),
                Transform::from_xyz(300.0, 0.0, 0.0),
            ),
        );
        app.update_n(1);
        assert!(
            audible(&app, outside),
            "0.5 outside beats 1.0 muffled to 0.1"
        );
        assert!(!audible(&app, muffled));

        let exempt = spawn(&mut app, (at_centre(3), UndampedSound));
        app.update_n(1);
        assert!(
            audible(&app, exempt),
            "an undamped sound keeps its full level"
        );
    }

    #[test]
    fn a_loop_beyond_its_max_distance_waits_until_the_listener_comes_close() {
        let mut app = queue_app(4, VirtualVoiceBudget::new());
        let listener = listener_at(&mut app, Vec2::ZERO);
        let distant = spawn(
            &mut app,
            (
                sound(1).looping().spatial().with_max_distance(100.0),
                Transform::from_xyz(500.0, 0.0, 0.0),
            ),
        );
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 1, "a loop is never culled");
        assert!(!audible(&app, distant));

        let moved = Transform::from_xyz(450.0, 0.0, 0.0);
        app.world_mut()
            .entity_mut(listener)
            .insert((moved, GlobalTransform::from(moved)));
        app.update_n(1);
        assert!(audible(&app, distant));
    }

    #[test]
    fn an_unpropagated_parent_counts_as_close() {
        let mut app = queue_app(4, VirtualVoiceBudget::new());
        listener_at(&mut app, Vec2::ZERO);
        let far = Transform::from_xyz(5000.0, 0.0, 0.0);
        let request =
            |parent| PlayQueuedAudio::new(sound(1).with_max_distance(100.0)).with_parent(parent);

        // A parent known to be far away culls its one-shot...
        let settled = spawn(&mut app, (far, GlobalTransform::from(far)));
        app.update_n(1);
        app.world_mut().write_message(request(settled));
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 0);

        // ...one spawned this frame has not been placed yet, and is kept.
        let fresh = spawn(&mut app, (far, GlobalTransform::from(far)));
        app.world_mut().write_message(request(fresh));
        app.update_n(1);
        assert_eq!(audible_handles(&mut app), vec![handle(1)]);
    }

    // ==================== Admission controls ====================

    /// A budget under which one-shots wait for the whole test.
    fn waiting() -> VirtualVoiceBudget<TestSound> {
        VirtualVoiceBudget::new().with_max_wait(Duration::from_secs(3600))
    }

    #[test]
    fn the_frame_budget_caps_one_shots_and_never_loops() {
        let mut app = queue_app(0, waiting().with_max_admissions_per_frame(1));
        for id in 1..=3 {
            play(&mut app, sound(id));
        }
        play(&mut app, sound(4).looping());
        play(&mut app, sound(5).looping());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 3, "one one-shot and both loops");

        // The cap is per frame, not cumulative.
        play(&mut app, sound(6));
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 4);
    }

    #[test]
    fn a_saturated_frame_admits_the_higher_tier_first() {
        let mut app = queue_app(0, waiting().with_max_admissions_per_frame(1));
        play(&mut app, sound(1));
        play(&mut app, sound(2).with_tier(3));
        app.update_n(1);

        let world = app.world_mut();
        let admitted: Vec<i32> = world
            .query::<&VirtualSound<TestSound>>()
            .iter(world)
            .map(VirtualSound::tier)
            .collect();
        assert_eq!(admitted, vec![3]);
    }

    #[test]
    fn per_sound_per_frame_cap_only_limits_that_sample() {
        let mut app = queue_app(0, waiting());
        for _ in 0..3 {
            play(&mut app, sound(1).with_max_per_frame(1));
        }
        play(&mut app, sound(2));
        app.update_n(1);

        let world = app.world_mut();
        let mut handles: Vec<Handle<AudioSample>> = world
            .query::<&VirtualSound<TestSound>>()
            .iter(world)
            .map(|sound| sound.handle.clone())
            .collect();
        handles.sort_by_key(Handle::id);
        assert_eq!(handles, vec![handle(1), handle(2)]);
    }

    #[test]
    fn max_concurrent_counts_live_entries_across_frames() {
        let mut app = queue_app(0, waiting());
        let request = || sound(1).with_max_concurrent(1);

        // Two same-frame requests: the second already sees the first.
        play(&mut app, request());
        play(&mut app, request());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 1);

        // A later request still sees the live entry.
        play(&mut app, request());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 1);

        // Once the entry is gone, the next request is admitted again.
        app.world_mut()
            .write_message(StopQueuedAudio::<TestSound>::all());
        app.update_n(1);
        play(&mut app, request());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 1);
    }

    #[test]
    fn min_repeat_interval_rejects_rapid_repeats() {
        let mut app = queue_app(0, waiting());
        let request = || sound(1).with_min_repeat_interval(Duration::from_millis(100));

        play(&mut app, request());
        app.update_n(1);
        // Immediately re-requested: inside the interval, dropped.
        play(&mut app, request());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 1);

        // Past the interval (fixed steps are ~15.6ms), admitted again.
        app.update_n(8);
        play(&mut app, request());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 2);
    }

    #[test]
    fn max_distance_culls_far_one_shots_when_a_listener_exists() {
        let mut app = queue_app(0, waiting());
        listener_at(&mut app, Vec2::ZERO);

        let request = |id: u128, position: Vec2| {
            PlayQueuedAudio::new(sound(id).with_max_distance(100.0)).at(position)
        };
        app.world_mut()
            .write_message(request(1, Vec2::new(50.0, 0.0)));
        app.world_mut()
            .write_message(request(2, Vec2::new(500.0, 0.0)));
        app.update_n(1);

        let world = app.world_mut();
        let handles: Vec<Handle<AudioSample>> = world
            .query::<&VirtualSound<TestSound>>()
            .iter(world)
            .map(|sound| sound.handle.clone())
            .collect();
        assert_eq!(handles, vec![handle(1)], "the far request is culled");
    }

    #[test]
    fn max_distance_never_culls_without_a_listener_or_position() {
        let mut app = queue_app(0, waiting());

        // No listener in the world: nothing to measure from, nothing culled.
        app.world_mut().write_message(
            PlayQueuedAudio::new(sound(1).with_max_distance(10.0)).at(Vec2::new(5000.0, 0.0)),
        );
        // A positionless request has nowhere to be measured from either.
        play(&mut app, sound(2).with_max_distance(10.0));
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 2);
    }

    fn recorded_admissions(app: &App) -> usize {
        app.world()
            .resource::<AdmissionState<TestSound>>()
            .last_admitted
            .len()
    }

    #[test]
    fn admission_bookkeeping_stays_bounded() {
        let mut app = queue_app(0, waiting());

        // Interval-less admissions are not recorded at all.
        for id in 1..=8 {
            play(&mut app, sound(id));
        }
        app.update_n(1);
        assert_eq!(recorded_admissions(&app), 0);

        // Recorded admissions past the prune threshold shed every entry too
        // old to block a request again.
        for id in 1..=80 {
            play(
                &mut app,
                sound(id).with_min_repeat_interval(Duration::from_millis(1)),
            );
        }
        app.update_n(1);
        assert_eq!(recorded_admissions(&app), 80);

        // A frame later everything recorded is older than its interval; the
        // next recorded admission prunes the lot.
        app.update_n(1);
        play(
            &mut app,
            sound(1000).with_min_repeat_interval(Duration::from_millis(1)),
        );
        app.update_n(1);
        assert_eq!(recorded_admissions(&app), 1);
    }

    // ==================== Volume model ====================

    #[test]
    fn a_config_change_keeps_a_promoted_entry_s_own_volume() {
        let mut app = queue_app(1, VirtualVoiceBudget::new());
        app.add_plugins(crate::MsgSeedlingPlugin::<TestSound>::default());
        play(&mut app, sound(1).looping().with_volume(0.25));
        app.update_n(1);

        let voice = the_voice(&mut app);
        assert_eq!(
            app.world().get::<BaseVolume>(voice).copied(),
            Some(BaseVolume(0.25)),
            "promotion records the entry's own level"
        );

        // `MsgSeedlingPlugin` now reaches this voice through its bare `C`.
        app.world_mut().resource_mut::<TestConfig>().sfx = 0.8;
        app.update_n(1);
        let volume = voice_volume(&app, voice);
        assert!(
            (volume - 0.2).abs() < 1e-6,
            "a settings change scales the entry's own volume, it does not \
             flatten every promoted voice to the category level, got {volume}"
        );
    }

    #[test]
    fn a_released_voice_sheds_its_baselines() {
        let mut app = queue_app(1, VirtualVoiceBudget::new());
        play(
            &mut app,
            sound(1).looping().with_volume(0.25).with_pitch(1.2),
        );
        app.update_n(1);
        let voice = the_voice(&mut app);

        // Flushed without another update: a further frame would simply
        // re-promote the entry (it is still the only one, and the lane has
        // room), putting the baselines straight back.
        {
            let world = app.world_mut();
            release_voice::<TestSound>(&mut world.commands(), voice, false, None);
            world.flush();
        }

        let world = app.world();
        assert!(world.get::<BaseVolume>(voice).is_none());
        assert!(world.get::<BasePitch>(voice).is_none());
        let entry = world
            .get::<VirtualSound<TestSound>>(voice)
            .expect("entry survives");
        assert!(
            (entry.base_volume() - 0.25).abs() < 1e-6 && (entry.pitch() - 1.2).abs() < 1e-6,
            "the entry's authored volume and pitch survive for the next promotion"
        );
    }

    #[test]
    fn promoted_voices_are_reached_by_ducking() {
        use crate::damping::DampingPlugin;
        use crate::ducking::{DuckingEnvelope, Ducks};

        let mut app = queue_app(1, VirtualVoiceBudget::new());
        app.add_plugins(DampingPlugin::<TestSound>::default());
        play(&mut app, sound(1).looping());
        app.update_n(1);

        let voice = the_voice(&mut app);
        assert!(
            app.world().get::<TestSound>(voice).is_some(),
            "promotion inserts the bare category component"
        );

        app.world_mut().entity_mut(voice).insert(Ducks);
        app.world_mut().resource_mut::<DuckingEnvelope>().trigger();
        app.update_n(5);

        let ducked_gain = app.world().resource::<DuckingEnvelope>().ducked_gain;
        let volume = voice_volume(&app, voice);
        assert!(
            (volume - ducked_gain).abs() < 1e-6,
            "the promoted voice ducks with the envelope, got {volume}"
        );
    }

    // ==================== Pool routing ====================

    #[test]
    fn default_routing_splits_spatial_and_non_spatial() {
        let mut app = physics_app();
        app.add_plugins(VirtualVoiceQueuePlugin::<TestSound>::default());
        play(&mut app, sound(1).looping());
        app.world_mut()
            .write_message(PlayQueuedAudio::new(sound(2).looping()).at(Vec2::new(1.0, 2.0)));
        app.update_n(1);

        let world = app.world_mut();
        let mut promoted = 0;
        let mut query = world.query_filtered::<(
            &VirtualSound<TestSound>,
            Has<DefaultPool>,
            Has<SpatialPool>,
        ), With<Audible>>();
        for (sound, default_pool, spatial_pool) in query.iter(world) {
            promoted += 1;
            if sound.is_spatial() {
                assert!(spatial_pool && !default_pool, "positioned voice");
            } else {
                assert!(default_pool && !spatial_pool, "positionless voice");
            }
        }
        assert_eq!(promoted, 2);
    }

    #[test]
    fn a_custom_lane_routes_promotion_and_release() {
        #[derive(Component, Clone)]
        struct CustomPool;

        let mut app = physics_app();
        app.add_plugins(
            VirtualVoiceQueuePlugin::<TestSound>::new()
                .with_budget(VirtualVoiceBudget::new().with_crossfade(Duration::ZERO))
                .with_pool_router(QueuePoolRouter::new(
                    vec![QueueLane::new(CustomPool, 1)],
                    |_| 0,
                )),
        );
        let voice = spawn(&mut app, sound(1).looping().with_volume(0.5));
        app.update_n(1);
        assert!(audible(&app, voice));
        assert!(app.world().get::<CustomPool>(voice).is_some());
        assert!(app.world().get::<DefaultPool>(voice).is_none());

        // Displace it; once the demotion fade completes, the release hook
        // strips the custom label along with the rest of the voice.
        play(&mut app, sound(2).looping());
        app.update_n(60);
        assert!(
            app.world().get_entity(voice).is_ok(),
            "loop returns to virtual"
        );
        assert!(!audible(&app, voice));
        assert!(app.world().get::<CustomPool>(voice).is_none());
    }

    #[test]
    fn a_lane_the_router_does_not_have_drops_the_entry() {
        let mut app = physics_app();
        app.add_plugins(
            VirtualVoiceQueuePlugin::<TestSound>::new().with_pool_router(QueuePoolRouter::new(
                vec![QueueLane::new(DefaultPool, 1)],
                |_| 5,
            )),
        );
        play(&mut app, sound(1).looping());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 0);
    }

    // ==================== Pool-agnostic admission gate ====================

    /// Admits with an empty environment: no live entries, no listener.
    fn admit(
        gate: &mut AdmissionGate<'_, TestSound>,
        request: &AdmissionRequest,
    ) -> Result<(), AdmissionRejection> {
        gate.try_admit(request, || 0, |_| None)
    }

    #[test]
    fn gate_frame_budget_caps_across_samples() {
        let mut state = AdmissionState::<TestSound>::default();
        let mut gate = AdmissionGate::new(&mut state, Some(2), Duration::ZERO);

        assert_eq!(
            admit(&mut gate, &AdmissionRequest::new(handle(1).id())),
            Ok(())
        );
        assert_eq!(
            admit(&mut gate, &AdmissionRequest::new(handle(2).id())),
            Ok(())
        );
        assert_eq!(
            admit(&mut gate, &AdmissionRequest::new(handle(3).id())),
            Err(AdmissionRejection::FrameBudgetSpent)
        );
        assert_eq!(gate.admitted(), 2);
    }

    #[test]
    fn gate_per_frame_cap_is_per_key() {
        let mut state = AdmissionState::<TestSound>::default();
        let mut gate = AdmissionGate::new(&mut state, None, Duration::ZERO);
        let capped = AdmissionRequest::new(handle(1).id()).with_max_per_frame(1);

        assert_eq!(admit(&mut gate, &capped), Ok(()));
        assert_eq!(
            admit(&mut gate, &capped),
            Err(AdmissionRejection::PerFrameCap)
        );
        // Another sample is untouched by the first one's cap.
        assert_eq!(
            admit(
                &mut gate,
                &AdmissionRequest::new(handle(2).id()).with_max_per_frame(1)
            ),
            Ok(())
        );
    }

    #[test]
    fn gate_concurrent_cap_counts_live_plus_this_frame() {
        let mut state = AdmissionState::<TestSound>::default();
        let mut gate = AdmissionGate::new(&mut state, None, Duration::ZERO);
        let request = AdmissionRequest::new(handle(1).id()).with_max_concurrent(2);

        // One live from before this frame, none admitted yet: room for one.
        assert_eq!(gate.try_admit(&request, || 1, |_| None), Ok(()));
        // The gate's own admission now counts toward the cap.
        assert_eq!(
            gate.try_admit(&request, || 1, |_| None),
            Err(AdmissionRejection::ConcurrentCap)
        );
    }

    #[test]
    fn gate_repeat_interval_spans_frames_and_ignores_interval_less_admissions() {
        let mut state = AdmissionState::<TestSound>::default();
        let plain = AdmissionRequest::new(handle(1).id());
        let spaced = AdmissionRequest::new(handle(1).id())
            .with_min_repeat_interval(Duration::from_millis(100));

        let mut gate = AdmissionGate::new(&mut state, None, Duration::ZERO);
        // An interval-less admission does not start the clock...
        assert_eq!(admit(&mut gate, &plain), Ok(()));
        // ...so a spaced request right after it is still admitted.
        assert_eq!(admit(&mut gate, &spaced), Ok(()));

        // The next frame is inside the interval; a later one is past it.
        let mut gate = AdmissionGate::new(&mut state, None, Duration::from_millis(50));
        assert_eq!(
            admit(&mut gate, &spaced),
            Err(AdmissionRejection::RepeatTooSoon)
        );
        let mut gate = AdmissionGate::new(&mut state, None, Duration::from_millis(150));
        assert_eq!(admit(&mut gate, &spaced), Ok(()));
    }

    #[test]
    fn gate_distance_culls_only_measurable_far_requests() {
        let mut state = AdmissionState::<TestSound>::default();
        let mut gate = AdmissionGate::new(&mut state, None, Duration::ZERO);
        let request = AdmissionRequest::new(handle(1).id())
            .at(Vec2::new(500.0, 0.0))
            .with_max_distance(100.0);

        assert_eq!(
            gate.try_admit(&request, || 0, |position| Some(position.length())),
            Err(AdmissionRejection::TooFar)
        );
        // No listener to measure against: never culled.
        assert_eq!(gate.try_admit(&request, || 0, |_| None), Ok(()));
        // A positionless request has nowhere to be measured from either.
        let positionless = AdmissionRequest::new(handle(2).id()).with_max_distance(100.0);
        assert_eq!(
            gate.try_admit(&positionless, || 0, |_| Some(f32::INFINITY)),
            Ok(())
        );
    }

    #[test]
    fn gate_queries_the_environment_only_for_set_controls() {
        let mut state = AdmissionState::<TestSound>::default();
        let mut gate = AdmissionGate::new(&mut state, None, Duration::ZERO);
        let request = AdmissionRequest::new(handle(1).id());

        let admitted = gate.try_admit(
            &request,
            || unreachable!("no max_concurrent set"),
            |_| unreachable!("no max_distance set"),
        );
        assert_eq!(admitted, Ok(()));
    }

    // ==================== Displacement query ====================

    #[test]
    fn displacement_picks_the_weakest_beyond_the_margin() {
        let candidates = [
            DisplacementCandidate::new(eid(0), 1.0),
            DisplacementCandidate::new(eid(1), 0.5),
        ];

        // Inside the margin over the weakest: no displacement.
        assert_eq!(displacement_target(&candidates, &(), 0.6, 1.25), None);
        // Beyond it: the weakest goes, not the loud one.
        assert_eq!(
            displacement_target(&candidates, &(), 0.7, 1.25),
            Some(eid(1))
        );
    }

    #[test]
    fn displacement_of_nothing_is_none() {
        let candidates: [DisplacementCandidate; 0] = [];
        assert_eq!(displacement_target(&candidates, &(), 10.0, 1.0), None);
    }

    #[test]
    fn displacement_tier_gate_beats_any_significance() {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        enum Tier {
            Low,
            Normal,
            Critical,
        }

        let candidates = [
            DisplacementCandidate::new(eid(0), 100.0).with_tier(Tier::Low),
            DisplacementCandidate::new(eid(1), 0.1).with_tier(Tier::Critical),
        ];

        // The weakest is the loud Low voice, not the quiet Critical one, and
        // a higher-tier newcomer takes it however quiet the newcomer is.
        assert_eq!(
            displacement_target(&candidates, &Tier::Critical, 0.05, 1.25),
            Some(eid(0))
        );
        assert_eq!(
            displacement_target(&candidates, &Tier::Normal, 0.0, 1.25),
            Some(eid(0))
        );

        // And a lower tier never displaces a higher one, however loud.
        let critical_only = [DisplacementCandidate::new(eid(1), 0.1).with_tier(Tier::Critical)];
        assert_eq!(
            displacement_target(&critical_only, &Tier::Low, 1000.0, 1.0),
            None
        );
    }

    #[test]
    fn displacement_same_tier_uses_the_margin() {
        let candidates = [DisplacementCandidate::new(eid(0), 1.0).with_tier(1u8)];

        assert_eq!(displacement_target(&candidates, &1u8, 1.2, 1.25), None);
        assert_eq!(
            displacement_target(&candidates, &1u8, 1.3, 1.25),
            Some(eid(0))
        );
    }

    #[test]
    fn displacement_margin_is_sanitized() {
        let candidates = [DisplacementCandidate::new(eid(0), 1.0)];

        // Non-finite and sub-1.0 margins behave as 1.0...
        assert_eq!(
            displacement_target(&candidates, &(), 1.01, f32::NAN),
            Some(eid(0))
        );
        assert_eq!(
            displacement_target(&candidates, &(), 1.01, 0.0),
            Some(eid(0))
        );
        // ...and equal significance never displaces, even at margin 1.0.
        assert_eq!(displacement_target(&candidates, &(), 1.0, 1.0), None);
    }

    #[test]
    fn displacement_weakest_ties_resolve_by_input_order() {
        let candidates = [
            DisplacementCandidate::new(eid(0), 0.5),
            DisplacementCandidate::new(eid(1), 0.5),
        ];
        assert_eq!(
            displacement_target(&candidates, &(), 1.0, 1.0),
            Some(eid(0))
        );
    }
}
