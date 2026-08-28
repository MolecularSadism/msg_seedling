//! Significance-ranked virtual voice queue.
//!
//! A fixed-size pool of real sampler voices (`bevy_seedling`'s own pool
//! machinery) has no idea which currently-playing sound matters most to the
//! player — when it runs out of room it steals whichever voice loses an
//! internal priority tie-break, with an instant cut and no fade. This module
//! adds a layer in front of that: every [`PlayQueuedAudio`] request becomes a
//! *virtual* entry ranked by **audible significance** (how loud it would
//! actually be, times an optional caller-supplied priority weight), and only
//! the top [`VirtualVoiceBudget::max_audible`] entries ever get a real
//! `SamplePlayer`. When ranking changes — a more significant sound arrives,
//! or a currently-audible one finishes — entries cross the line smoothly via
//! [`FadeInAudio`]/[`FadeOutAudio`] instead of a hard cut, so two sounds
//! trading places actually crossfades.
//!
//! Entries that don't make the cut aren't rejected outright: they wait as
//! silent virtual entries and get promoted the moment a slot frees up or
//! they become the most significant thing requested. Looping entries wait
//! indefinitely; one-shots give up after [`VirtualVoiceBudget::max_wait`] —
//! a one-shot promoted long after its cause would play out of sync with it.
//!
//! # Voice lifecycle
//!
//! A promoted entry owns a real `SamplePlayer` carrying an elevated
//! [`VirtualVoiceBudget::sample_priority`], so default-priority one-shots
//! (e.g. plain [`PlayAudio`](crate::PlayAudio) requests sharing the pool)
//! cannot steal a voice the queue considers significant. Demotion is not the
//! end of the sound:
//!
//! - **Looping** entries fade out, shed their `SamplePlayer`, and return to
//!   the silent virtual state, eligible for re-promotion indefinitely.
//!   Playback restarts from the beginning when re-promoted.
//! - **One-shot** entries despawn once their demotion fade completes — a
//!   partially-played one-shot restarting later would be wrong.
//!
//! A promoted voice that ends inside seedling — stolen by a higher-priority
//! sample, expired in the sampler queue, or (for one-shots) finished playing
//! — is reclaimed under the same policy: looping entries return to virtual,
//! one-shots despawn. Size the target pool so `max_audible` plus the
//! crossfades you expect in flight fit within its `PoolSize`: during a
//! crossfade the outgoing and incoming voices briefly coexist.
//!
//! # Scope
//!
//! A promoted entry carries the bare `C` component for as long as it holds a
//! real voice, so the crate's per-sound category systems reach queue voices
//! like any other sound of the category: [`damping`](crate::damping) fields
//! and the [`ducking`](crate::ducking) envelope, and — when
//! [`MsgSeedlingPlugin`](crate::MsgSeedlingPlugin) is added for the same `C`
//! — [`StopAudio`](crate::StopAudio), [`FadeAudio`](crate::FadeAudio), and
//! the config-driven volume rewrites too. Waiting (virtual) entries carry no
//! `C`, and a demoted loop sheds it along with its voice, so prefer
//! [`StopQueuedAudio`] for queue entries: it is the only stop that also
//! reaches the waiting ones, and it fades audible ones out through the
//! queue's own crossfade instead of cutting them. Significance tracks the
//! category volume from `C::Config` live: a config change re-ranks waiting
//! and audible entries on the next frame, and promotions fade in at the
//! freshly computed volume.
//!
//! Unlike [`PlayAudio`](crate::PlayAudio), [`PlayQueuedAudio`] has no
//! [`Randomization`](crate::Randomization) support. Significance also does
//! not factor in distance for spatial sounds — bake any distance attenuation
//! into [`PlayQueuedAudio::with_volume`] or
//! [`PlayQueuedAudio::with_priority`] before sending, if the caller can
//! compute it (e.g. from a listener position it already tracks).
//!
//! # Bring your own pool
//!
//! The queue's decision layer is public for hosts that drive their own
//! sampler pools instead of adding [`VirtualVoiceQueuePlugin`]:
//!
//! - [`AdmissionGate`] runs the admission controls — the global per-frame
//!   budget plus [`AdmissionRequest`]'s per-sample caps, repeat interval and
//!   distance cull — over the same [`AdmissionState`] resource the plugin
//!   uses. The plugin's own enqueue system is built on it, so a host gating
//!   its own request stream gets identical semantics.
//! - [`rank_by_significance`] answers "which of this whole set should be
//!   audible".
//! - [`displacement_target`] answers the pairwise question instead — "does
//!   this newcomer outrank the weakest voice it may displace" — with an
//!   optional hard tier ordering above significance.
//!
//! A host that only needs a different promotion *target* — its own pool
//! labels, its own effects chain — keeps the whole plugin and swaps the
//! routing via [`VirtualVoiceQueuePlugin::with_pool_router`].
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
//! fn play_sounds(mut writer: MessageWriter<PlayQueuedAudio<Sound>>) {
//!     // A quiet looping ambience: waits as virtual while the queue is
//!     // full of louder things, promoted when room frees up.
//!     writer.write(
//!         PlayQueuedAudio::new(Handle::default(), Sound::Ambience)
//!             .looping()
//!             .with_volume(0.2),
//!     );
//!
//!     // A loud explosion: outranks quieter voices and crossfades in,
//!     // smoothly displacing whichever currently-audible voice ranks lowest.
//!     writer.write(PlayQueuedAudio::new(Handle::default(), Sound::Sfx).with_priority(2.0));
//! }
//!
//! let mut app = App::new();
//! app.add_plugins(MinimalPlugins);
//! app.add_plugins(VirtualVoiceQueuePlugin::<Sound>::default());
//! app.add_systems(Update, play_sounds);
//! app.update();
//! ```

use core::cmp::Ordering;
use core::time::Duration;
use std::sync::Arc;

use bevy::ecs::entity::Entities;
use bevy::ecs::system::EntityCommands;
use bevy::prelude::*;
use bevy_seedling::pool::label::PoolLabelContainer;
use bevy_seedling::pool::{CompletionReason, Sampler};
use bevy_seedling::prelude::*;
// Explicit: with bevy's `bevy_audio` feature on, `bevy::prelude` exports a
// `PlaybackSettings` of its own and the two globs would otherwise resolve to it.
use bevy_seedling::prelude::PlaybackSettings;
use bevy_seedling::sample::{AudioSample, QueuedSample};

use crate::baseline::{BasePitch, BaseVolume, sanitize_weight};
use crate::fade::{FadeInAudio, FadeOutAudio, FadeSystems};
use crate::messages::SpatialPosition;
use crate::traits::AudioCategory;

const DEFAULT_MAX_AUDIBLE: usize = 16;
const DEFAULT_CROSSFADE: Duration = Duration::from_millis(50);
const DEFAULT_MAX_WAIT: Duration = Duration::from_millis(500);
const DEFAULT_SAMPLE_PRIORITY: i32 = 2;

/// Default incumbent-bonus displacement margin (~2 dB): a newcomer must be
/// this factor more significant than an audible voice to displace it. Used
/// by [`VirtualVoiceBudget::displacement_margin`], and the value to reach
/// for as [`displacement_target`]'s `margin` unless you have a reason not
/// to.
pub const DEFAULT_DISPLACEMENT_MARGIN: f32 = 1.25;

/// Budget for one category type's significance-ranked voice queue.
///
/// Scoped per `C` so unrelated category types (e.g. music vs. SFX) never
/// compete for the same slots when both use [`VirtualVoiceQueuePlugin`].
#[derive(Resource, Clone, Debug)]
pub struct VirtualVoiceBudget<C: AudioCategory> {
    /// Maximum number of entries that may hold a real `SamplePlayer` voice
    /// at once. The target pool's `PoolSize` must accommodate this plus the
    /// crossfades expected in flight.
    pub max_audible: usize,
    /// Duration of the crossfade applied when an entry is promoted or
    /// demoted.
    pub crossfade: Duration,
    /// How long a never-promoted one-shot entry waits before it is dropped.
    /// Looping entries wait indefinitely.
    pub max_wait: Duration,
    /// Seedling `SamplePriority` given to promoted voices, so
    /// default-priority (0) one-shots sharing the pool cannot steal them.
    pub sample_priority: i32,
    /// Global cap on how many requests may be admitted into the queue per
    /// frame, across every sound. `None` (the default) admits everything.
    /// Requests past the cap are dropped, not deferred — a one-shot delayed
    /// a frame would play out of sync with its cause, and its sender can
    /// re-request.
    pub max_admissions_per_frame: Option<usize>,
    /// Incumbent bonus: a newcomer must exceed an audible voice's
    /// significance by this factor to displace it. The default `1.25`
    /// (~2 dB) is hysteresis against crossfade flutter when two sounds
    /// hover near equal loudness — without it an epsilon-louder newcomer
    /// churns demotions, and each loop demotion restarts playback from the
    /// beginning. Applied by multiplying audible entries' significance
    /// before [`rank_by_significance`], whose own semantics stay
    /// margin-unaware. Sanitized to `>= 1.0` (`1.0` = no hysteresis).
    pub displacement_margin: f32,
    _phantom: core::marker::PhantomData<C>,
}

impl<C: AudioCategory> VirtualVoiceBudget<C> {
    /// Creates a budget for up to `max_audible` simultaneous real voices,
    /// using the default crossfade (50ms), max wait (500ms), sample
    /// priority (2), and displacement margin (1.25).
    #[must_use]
    pub fn new(max_audible: usize) -> Self {
        Self {
            max_audible,
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

    /// Caps how many requests may be admitted into the queue per frame.
    #[must_use]
    pub fn with_max_admissions_per_frame(mut self, cap: usize) -> Self {
        self.max_admissions_per_frame = Some(cap);
        self
    }

    /// Sets the incumbent-bonus displacement margin, sanitized to `>= 1.0`.
    #[must_use]
    pub fn with_displacement_margin(mut self, displacement_margin: f32) -> Self {
        self.displacement_margin = if displacement_margin.is_finite() {
            displacement_margin.max(1.0)
        } else {
            1.0
        };
        self
    }
}

impl<C: AudioCategory> Default for VirtualVoiceBudget<C> {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_AUDIBLE)
    }
}

/// A sound tracked by the virtual voice queue.
///
/// Lives on one entity for the sound's whole logical lifetime: present
/// without [`Audible`] while virtual (no real voice, nothing plays), present
/// with [`Audible`] once promoted.
#[derive(Component, Clone, Reflect)]
#[reflect(Component)]
pub struct VirtualSound<C: AudioCategory> {
    handle: Handle<AudioSample>,
    category: C,
    looping: bool,
    parent: Option<Entity>,
    position: Option<SpatialPosition>,
    /// Request volume before the category multiplier; the effective target
    /// is recomputed from `C::Config` every frame.
    base_volume: f32,
    priority: f32,
    requested_at: Duration,
}

impl<C: AudioCategory> VirtualSound<C> {
    /// The category this sound was requested under.
    #[must_use]
    pub fn category(&self) -> C {
        self.category
    }

    /// Whether this sound loops once promoted.
    #[must_use]
    pub fn looping(&self) -> bool {
        self.looping
    }

    /// The sample this sound plays.
    #[must_use]
    pub fn handle(&self) -> &Handle<AudioSample> {
        &self.handle
    }

    /// The parent entity this sound attaches to, if any.
    #[must_use]
    pub fn parent(&self) -> Option<Entity> {
        self.parent
    }

    /// The spatial position this sound was requested at, if any.
    #[must_use]
    pub fn position(&self) -> Option<SpatialPosition> {
        self.position
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

/// Message: request a sound through the significance-ranked virtual voice
/// queue instead of playing unconditionally.
///
/// Unlike [`PlayAudio`](crate::PlayAudio), this carries no
/// [`Randomization`](crate::Randomization) support.
#[derive(Message, Clone)]
pub struct PlayQueuedAudio<C: AudioCategory> {
    pub handle: Handle<AudioSample>,
    pub category: C,
    pub looping: bool,
    pub parent: Option<Entity>,
    pub position: Option<SpatialPosition>,
    /// Base volume multiplier (before category/master). Default: `1.0`.
    pub volume: f32,
    /// Priority weight multiplying audible significance. Default: `1.0`.
    /// Two requests at equal volume with different priority always resolve
    /// by priority; use this for game-meaning importance a raw volume
    /// number can't express (e.g. a story beat's cue over ambient chatter).
    pub priority: f32,
    /// Cap on live queue entries (virtual or audible, retiring excluded)
    /// playing this same sample; the request is dropped at the cap.
    /// `None` (the default) never caps.
    pub max_concurrent: Option<usize>,
    /// Cap on admissions of this same sample within one frame; the request
    /// is dropped at the cap. `None` (the default) never caps.
    pub max_per_frame: Option<usize>,
    /// Minimum time since this same sample was last admitted; a request
    /// arriving sooner is dropped. Only admissions that themselves set an
    /// interval are tracked — interval-less admissions of the sample do not
    /// reset the clock, which also keeps the bookkeeping bounded. `None`
    /// (the default) never limits.
    pub min_repeat_interval: Option<Duration>,
    /// Maximum distance from the nearest spatial listener at which this
    /// request is worth admitting; a positioned request farther away is
    /// culled. Requests without a [`PlayQueuedAudio::position`], and apps
    /// without a listener, are never culled. `None` (the default) never
    /// culls. [`Self::with_max_distance`] sanitizes the value.
    ///
    /// Measured in the XY plane against `SpatialListener2D` only, matching
    /// [`damping`](crate::damping#geometry) — a `SpatialListener3D` app has no
    /// listener to measure against here, so nothing is ever culled.
    pub max_distance: Option<f32>,
}

impl<C: AudioCategory> PlayQueuedAudio<C> {
    /// Creates a new queued-play request with default settings.
    #[must_use]
    pub fn new(handle: Handle<AudioSample>, category: C) -> Self {
        Self {
            handle,
            category,
            looping: false,
            parent: None,
            position: None,
            volume: 1.0,
            priority: 1.0,
            max_concurrent: None,
            max_per_frame: None,
            min_repeat_interval: None,
            max_distance: None,
        }
    }

    /// Sets playback to loop endlessly once promoted.
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

    /// Sets the priority weight multiplying audible significance.
    #[must_use]
    pub fn with_priority(mut self, priority: f32) -> Self {
        self.priority = priority;
        self
    }

    /// Caps live queue entries playing this same sample.
    #[must_use]
    pub fn with_max_concurrent(mut self, max_concurrent: usize) -> Self {
        self.max_concurrent = Some(max_concurrent);
        self
    }

    /// Caps admissions of this same sample within one frame.
    #[must_use]
    pub fn with_max_per_frame(mut self, max_per_frame: usize) -> Self {
        self.max_per_frame = Some(max_per_frame);
        self
    }

    /// Sets the minimum time since this same sample was last admitted —
    /// measured against previous admissions that themselves set an interval.
    #[must_use]
    pub fn with_min_repeat_interval(mut self, interval: Duration) -> Self {
        self.min_repeat_interval = Some(interval);
        self
    }

    /// Culls the request when its position is farther than this from the
    /// nearest `SpatialListener2D`, measured in the XY plane. Sanitized like
    /// the crate's other numeric inputs: a non-finite distance is discarded
    /// (never culls), a negative one clamps to `0.0`.
    #[must_use]
    pub fn with_max_distance(mut self, max_distance: f32) -> Self {
        self.max_distance = max_distance.is_finite().then_some(max_distance.max(0.0));
        self
    }

    /// The admission-control slice of this request, in the shape
    /// [`AdmissionGate::try_admit`] takes.
    #[must_use]
    pub fn admission_request(&self) -> AdmissionRequest {
        AdmissionRequest {
            sample: self.handle.id(),
            position: self.position.map(|position| position.as_vec3().truncate()),
            max_concurrent: self.max_concurrent,
            max_per_frame: self.max_per_frame,
            min_repeat_interval: self.min_repeat_interval,
            max_distance: self.max_distance,
        }
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

/// Where promoted voices go: the pool routing behind
/// [`VirtualVoiceQueuePlugin`], pluggable per category type.
///
/// The default routes exactly as the queue always has: spatial entries (a
/// parent or a position) into seedling's `SpatialPool`, the rest into
/// `DefaultPool`. A host with its own `PoolLabel` set — say a dampable pool
/// with a low-pass node in its chain and a critical pool without — swaps the
/// routing via [`VirtualVoiceQueuePlugin::with_pool_router`] and keeps every
/// other queue behavior:
///
/// - `route` runs at promotion, on the entry about to receive its
///   `SamplePlayer`: insert your pool label (and anything else the pool
///   expects) there, choosing by the entry's [`VirtualSound::category`],
///   [`VirtualSound::position`], and so on.
/// - `release` runs when the voice is released — a demotion fade completing,
///   a stop, or seedling ending the voice: remove what `route` inserted, so
///   the entry returns to a bare virtual state (label removal is cheap
///   whether or not this particular promotion inserted it, which is why the
///   default removes both built-in labels unconditionally).
#[derive(Resource, Clone)]
pub struct QueuePoolRouter<C: AudioCategory> {
    route: Arc<dyn Fn(&mut EntityCommands, &VirtualSound<C>) + Send + Sync>,
    release: Arc<dyn Fn(&mut EntityCommands) + Send + Sync>,
}

impl<C: AudioCategory> QueuePoolRouter<C> {
    /// Creates a router from a promotion-time `route` and its release-time
    /// inverse.
    pub fn new(
        route: impl Fn(&mut EntityCommands, &VirtualSound<C>) + Send + Sync + 'static,
        release: impl Fn(&mut EntityCommands) + Send + Sync + 'static,
    ) -> Self {
        Self {
            route: Arc::new(route),
            release: Arc::new(release),
        }
    }
}

impl<C: AudioCategory> Default for QueuePoolRouter<C> {
    fn default() -> Self {
        Self::new(
            |ec, sound| {
                if sound.parent.is_some() || sound.position.is_some() {
                    ec.insert(SpatialPool);
                } else {
                    ec.insert(DefaultPool);
                }
            },
            |ec| {
                ec.remove::<(SpatialPool, DefaultPool)>();
            },
        )
    }
}

/// Plugin adding a significance-ranked virtual voice queue for category `C`.
///
/// Independent of [`MsgSeedlingPlugin`](crate::MsgSeedlingPlugin) — add both
/// if you want direct [`PlayAudio`](crate::PlayAudio) playback *and* the
/// queue for the same category type. Pulls in [`crate::fade::plugin`]
/// automatically (safe to add alongside an explicit `fade::plugin`, or
/// alongside another category's `VirtualVoiceQueuePlugin` — registration is
/// idempotent).
///
/// Promoted voices carry [`VirtualVoiceBudget::sample_priority`], so when
/// the queue shares a pool with direct `PlayAudio` playback, its voices win
/// pool contention against default-priority one-shots. Configure the pool's
/// `PoolSize` to fit `max_audible` plus expected concurrent crossfades.
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
    /// Creates a new plugin with the default budget.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the voice budget.
    #[must_use]
    pub fn with_budget(mut self, budget: VirtualVoiceBudget<C>) -> Self {
        self.budget = budget;
        self
    }

    /// Sets where promoted voices go, replacing the default
    /// `SpatialPool`/`DefaultPool` routing.
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
        app.add_systems(
            Update,
            (
                (
                    reclaim_lost_voices::<C>,
                    finish_demotions::<C>,
                    enqueue_queued_audio::<C>,
                    handle_stop_queued_audio::<C>,
                ),
                rank_virtual_voices::<C>,
            )
                .chain()
                // Fade completions must be applied before queue decisions
                // read entry state, so a completing fade and a same-frame
                // stop cannot race.
                .after(FadeSystems),
        );
    }
}

/// Per-category admission bookkeeping: when each sample was last admitted,
/// for [`PlayQueuedAudio::min_repeat_interval`].
///
/// [`VirtualVoiceQueuePlugin`] initializes it; a bring-your-own-pool host
/// gating its own request stream through [`AdmissionGate`] initializes it
/// itself (`app.init_resource::<AdmissionState<C>>()`).
///
/// Bounded however many distinct samples stream through a session: only
/// admissions that carry an interval are recorded, and once the map grows
/// past a threshold, entries older than the longest interval seen — too old
/// to ever block a request again — are pruned before recording more.
#[derive(Resource)]
pub struct AdmissionState<C: AudioCategory> {
    last_admitted: bevy::platform::collections::HashMap<AssetId<AudioSample>, Duration>,
    /// The longest interval any recorded admission carried; the prune
    /// horizon.
    longest_interval: Duration,
    _phantom: core::marker::PhantomData<C>,
}

impl<C: AudioCategory> Default for AdmissionState<C> {
    fn default() -> Self {
        Self {
            last_admitted: Default::default(),
            longest_interval: Duration::ZERO,
            _phantom: core::marker::PhantomData,
        }
    }
}

impl<C: AudioCategory> AdmissionState<C> {
    /// Recorded admissions beyond this many trigger a prune of entries past
    /// the horizon before the next is recorded.
    const PRUNE_THRESHOLD: usize = 64;

    /// Records an admission of `id` at `now` under `interval`.
    fn record(&mut self, id: AssetId<AudioSample>, now: Duration, interval: Duration) {
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
/// [`PlayQueuedAudio`] so a bring-your-own-pool host can gate its own
/// request type through [`AdmissionGate`]. Every control defaults to off — a
/// request using none of them is always admitted (up to the gate's global
/// frame budget).
///
/// [`PlayQueuedAudio::admission_request`] produces one from a queue request;
/// each field mirrors the [`PlayQueuedAudio`] field of the same name.
#[derive(Clone, Debug)]
pub struct AdmissionRequest {
    /// The sample being requested; the per-sample controls key on its id.
    pub sample: AssetId<AudioSample>,
    /// XY world position, measured against listeners for
    /// [`Self::max_distance`]. `None` is never distance-culled.
    pub position: Option<Vec2>,
    /// Cap on live entries playing this same sample; see
    /// [`PlayQueuedAudio::max_concurrent`].
    pub max_concurrent: Option<usize>,
    /// Cap on admissions of this same sample within one frame; see
    /// [`PlayQueuedAudio::max_per_frame`].
    pub max_per_frame: Option<usize>,
    /// Minimum time since this same sample was last admitted *with an
    /// interval*; see [`PlayQueuedAudio::min_repeat_interval`].
    pub min_repeat_interval: Option<Duration>,
    /// Maximum distance from the nearest listener worth admitting at; see
    /// [`PlayQueuedAudio::max_distance`].
    pub max_distance: Option<f32>,
}

impl AdmissionRequest {
    /// Creates a request for `sample` with every control off.
    #[must_use]
    pub fn new(sample: AssetId<AudioSample>) -> Self {
        Self {
            sample,
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

    /// Caps live entries playing this same sample.
    #[must_use]
    pub fn with_max_concurrent(mut self, max_concurrent: usize) -> Self {
        self.max_concurrent = Some(max_concurrent);
        self
    }

    /// Caps admissions of this same sample within one frame.
    #[must_use]
    pub fn with_max_per_frame(mut self, max_per_frame: usize) -> Self {
        self.max_per_frame = Some(max_per_frame);
        self
    }

    /// Sets the minimum time since this same sample was last admitted.
    #[must_use]
    pub fn with_min_repeat_interval(mut self, interval: Duration) -> Self {
        self.min_repeat_interval = Some(interval);
        self
    }

    /// Culls the request when farther than this from the nearest listener.
    /// Sanitized like [`PlayQueuedAudio::with_max_distance`]: a non-finite
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
    /// This sample already hit its [`AdmissionRequest::max_per_frame`] this
    /// frame.
    PerFrameCap,
    /// This sample already has [`AdmissionRequest::max_concurrent`] live
    /// entries (same-frame admissions included).
    ConcurrentCap,
    /// This sample was last admitted less than
    /// [`AdmissionRequest::min_repeat_interval`] ago.
    RepeatTooSoon,
    /// The request's position is farther than
    /// [`AdmissionRequest::max_distance`] from the nearest listener.
    TooFar,
}

/// One frame's admission gate: the significance queue's admission controls,
/// pool-agnostic, for hosts gating their own request stream.
///
/// Create one per frame over the category's [`AdmissionState`] and feed it
/// each request in arrival order; [`Self::try_admit`] answers per request
/// and does the bookkeeping (frame counters, repeat-interval recording) for
/// the ones that pass. [`VirtualVoiceQueuePlugin`]'s own enqueue system is
/// implemented on this same gate, so a host running it against its own
/// requests gets identical semantics, control for control.
pub struct AdmissionGate<'a, C: AudioCategory> {
    state: &'a mut AdmissionState<C>,
    frame_budget: Option<usize>,
    now: Duration,
    admitted: usize,
    admitted_per_sample: bevy::platform::collections::HashMap<AssetId<AudioSample>, usize>,
}

impl<'a, C: AudioCategory> AdmissionGate<'a, C> {
    /// Opens the gate for one frame. `frame_budget` caps admissions across
    /// every sound this frame (the queue passes
    /// [`VirtualVoiceBudget::max_admissions_per_frame`]; `None` admits
    /// everything); `now` is the current [`Time::elapsed`].
    #[must_use]
    pub fn new(
        state: &'a mut AdmissionState<C>,
        frame_budget: Option<usize>,
        now: Duration,
    ) -> Self {
        Self {
            state,
            frame_budget,
            now,
            admitted: 0,
            admitted_per_sample: Default::default(),
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
    /// counts the live entries already playing the request's sample from
    /// *before* this frame's admissions — the gate adds this frame's own —
    /// where "live" for the queue means virtual or audible, retiring
    /// excluded; `nearest_listener_distance` is the distance from the given
    /// position to the nearest listener, `None` when there is no listener to
    /// measure against (which never culls). The queue measures XY distance
    /// to `SpatialListener2D`s; a host measures however its geometry works.
    pub fn try_admit(
        &mut self,
        request: &AdmissionRequest,
        live_count: impl FnOnce() -> usize,
        nearest_listener_distance: impl FnOnce(Vec2) -> Option<f32>,
    ) -> Result<(), AdmissionRejection> {
        if let Some(cap) = self.frame_budget
            && self.admitted >= cap
        {
            return Err(AdmissionRejection::FrameBudgetSpent);
        }

        let same_this_frame = self
            .admitted_per_sample
            .get(&request.sample)
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
            && let Some(&last) = self.state.last_admitted.get(&request.sample)
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
        *self.admitted_per_sample.entry(request.sample).or_insert(0) += 1;
        if let Some(interval) = request.min_repeat_interval {
            self.state.record(request.sample, self.now, interval);
        }
        Ok(())
    }
}

fn enqueue_queued_audio<C: AudioCategory>(
    mut commands: Commands,
    time: Res<Time>,
    budget: Res<VirtualVoiceBudget<C>>,
    mut admission: ResMut<AdmissionState<C>>,
    mut messages: MessageReader<PlayQueuedAudio<C>>,
    existing: Query<&VirtualSound<C>, Without<Retiring>>,
    q_listeners: Query<&GlobalTransform, With<SpatialListener2D>>,
) {
    let now = time.elapsed();
    let mut gate = AdmissionGate::new(&mut admission, budget.max_admissions_per_frame, now);
    // Resolved lazily: most frames have no distance-culled request.
    let mut listener_positions: Option<Vec<Vec2>> = None;

    for msg in messages.read() {
        let request = msg.admission_request();
        let admitted = gate.try_admit(
            &request,
            || {
                existing
                    .iter()
                    .filter(|sound| sound.handle.id() == request.sample)
                    .count()
            },
            |position| {
                listener_positions
                    .get_or_insert_with(|| {
                        q_listeners
                            .iter()
                            .map(|transform| transform.translation().truncate())
                            .collect()
                    })
                    .iter()
                    .map(|listener| listener.distance(position))
                    .min_by(f32::total_cmp)
            },
        );
        if admitted.is_err() {
            continue;
        }
        commands.spawn(VirtualSound {
            handle: msg.handle.clone(),
            category: msg.category,
            looping: msg.looping,
            parent: msg.parent,
            position: msg.position,
            base_volume: sanitize_weight(msg.volume),
            priority: sanitize_weight(msg.priority),
            requested_at: now,
        });
    }
}

/// One queue entry's ranking inputs, for [`rank_by_significance`].
#[derive(Clone, Copy, Debug)]
pub struct SignificanceEntry {
    pub entity: Entity,
    pub significance: f32,
    pub audible: bool,
}

/// What [`rank_by_significance`] decided an entry should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceDecision {
    /// Give this virtual entry a real voice — it ranks in the top
    /// `max_audible`, and doesn't have one yet.
    Promote,
    /// Take this entry's real voice away — it no longer ranks in the top
    /// `max_audible`, but currently has one.
    Demote,
    /// No change: either already in the right state, or still waiting for a
    /// slot to open up.
    Hold,
}

/// Pure ranking decision, exposed for callers who need their own promotion
/// target — a custom sampler pool, a custom effects chain (e.g. one with a
/// low-pass filter for muffling) — rather than [`VirtualVoiceQueuePlugin`]'s
/// built-in `SpatialPool`/`DefaultPool` routing. Combine with
/// [`crate::fade::FadeInAudio`]/[`crate::fade::FadeOutAudio`] (pool-agnostic
/// themselves — they only need `SampleEffects`) to get the same
/// promote/demote crossfade behavior on top of any pool.
///
/// Entries are ranked by `significance` descending (ties keep their input
/// order, so equally significant requests resolve deterministically by
/// arrival order). The top `max_audible` entries form the audible set:
///
/// - An audible entry in the set is [`VoiceDecision::Hold`] — it keeps its
///   voice. Voices already retiring (mid fade-out, passed as `retiring`)
///   never evict an audible hold.
/// - A virtual entry in the set is [`VoiceDecision::Promote`]d, but only
///   while the audible holds in the set, the `retiring` voices, and the
///   promotions granted so far stay under `max_audible`; otherwise it
///   [`VoiceDecision::Hold`]s until retiring fades free their slots.
/// - Below the set, audible entries are [`VoiceDecision::Demote`]d and
///   virtual entries [`VoiceDecision::Hold`] (still waiting).
///
/// An entry demoted this frame does not count against promotions — its
/// replacement fades in while it fades out, so a crossfade briefly holds one
/// voice more than `max_audible` per swap. Decisions are returned in input
/// order, one per entry. Significance is compared with `f32::total_cmp`, so
/// NaN never panics but sorts above every finite value — sanitize weights
/// upstream, as [`VirtualVoiceQueuePlugin`]'s enqueue path does.
#[must_use]
pub fn rank_by_significance(
    entries: &[SignificanceEntry],
    max_audible: usize,
    retiring: usize,
) -> Vec<(Entity, VoiceDecision)> {
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&a, &b| entries[b].significance.total_cmp(&entries[a].significance));

    let (top, rest) = order.split_at(max_audible.min(order.len()));

    let mut decisions = vec![VoiceDecision::Hold; entries.len()];
    let mut in_flight = top.iter().filter(|&&i| entries[i].audible).count() + retiring;
    for &i in top {
        if !entries[i].audible && in_flight < max_audible {
            in_flight += 1;
            decisions[i] = VoiceDecision::Promote;
        }
    }
    for &i in rest {
        if entries[i].audible {
            decisions[i] = VoiceDecision::Demote;
        }
    }

    entries
        .iter()
        .zip(decisions)
        .map(|(entry, decision)| (entry.entity, decision))
        .collect()
}

/// One audible voice as a candidate for [`displacement_target`].
///
/// `tier` is an optional *hard* priority layer above significance: a
/// higher-tier newcomer displaces a lower-tier voice regardless of how the
/// significances compare, and a lower-tier newcomer never displaces a
/// higher-tier voice, however loud — the guarantee a raw
/// [`PlayQueuedAudio::priority`] weight cannot make. The default `()` puts
/// every candidate in the same tier, leaving pure significance with
/// hysteresis.
#[derive(Clone, Copy, Debug)]
pub struct DisplacementCandidate<T = ()> {
    /// The voice's entity, returned when it is the displacement target.
    pub entity: Entity,
    /// The voice's audible significance (volume × priority weight),
    /// sanitized upstream like [`rank_by_significance`]'s inputs.
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

/// Answers the displacement question [`rank_by_significance`] does not: does
/// this *newcomer* outrank the weakest voice it may displace?
///
/// [`rank_by_significance`] decides which of a whole set should be audible.
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
/// candidate in input order, matching [`rank_by_significance`]'s tie rule.
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

fn rank_virtual_voices<C: AudioCategory>(
    mut commands: Commands,
    budget: Res<VirtualVoiceBudget<C>>,
    router: Res<QueuePoolRouter<C>>,
    config: Res<C::Config>,
    time: Res<Time>,
    entities: &Entities,
    eligible: Query<(Entity, &VirtualSound<C>, Has<Audible>), Without<Retiring>>,
    retiring: Query<(), (With<Retiring>, With<VirtualSound<C>>)>,
) {
    let retiring_count = retiring.iter().count();
    // `f32::max` also maps a NaN margin (written past the builder) to 1.0.
    let margin = budget.displacement_margin.max(1.0);

    let mut entries: Vec<SignificanceEntry> = Vec::new();
    let mut sounds: Vec<(&VirtualSound<C>, f32)> = Vec::new();
    for (entity, sound, audible) in &eligible {
        if let Some(parent) = sound.parent
            && !entities.contains(parent)
        {
            // A spatial sound of a dead emitter must not play at origin.
            commands.entity(entity).despawn();
            continue;
        }
        let target_volume = sanitize_weight(sound.category.volume(&config) * sound.base_volume);
        let significance = target_volume * sound.priority;
        entries.push(SignificanceEntry {
            entity,
            significance: if audible {
                significance * margin
            } else {
                significance
            },
            audible,
        });
        sounds.push((sound, target_volume));
    }

    if entries.is_empty() {
        return;
    }

    let decisions = rank_by_significance(&entries, budget.max_audible, retiring_count);
    let now = time.elapsed();

    for ((entity, decision), (entry, (sound, target_volume))) in
        decisions.into_iter().zip(entries.iter().zip(sounds.iter()))
    {
        match decision {
            VoiceDecision::Promote => {
                promote(
                    &mut commands,
                    entity,
                    sound,
                    *target_volume,
                    &budget,
                    &router,
                );
            }
            VoiceDecision::Demote => {
                demote(&mut commands, entity, sound.looping, budget.crossfade);
            }
            VoiceDecision::Hold => {
                if !entry.audible
                    && !sound.looping
                    && now.saturating_sub(sound.requested_at) > budget.max_wait
                {
                    commands.entity(entity).despawn();
                }
            }
        }
    }
}

/// Gives an entry a real voice. `SamplePriority` is inserted at the budget's
/// elevated value on every promotion, overwriting the `SamplePriority(0)` a
/// demotion drops the entry to, so a re-promoted loop defends its voice
/// again. The bare `C` component rides along so per-sound category systems
/// (damping, ducking, the config-driven volume rewrites) see the voice, and
/// [`BaseVolume`]/[`BasePitch`] ride along with it so those systems recompute
/// the entry's *own* level rather than flattening it to the bare category
/// volume; [`release_voice`] strips all three with the rest of the promotion.
fn promote<C: AudioCategory>(
    commands: &mut Commands,
    entity: Entity,
    sound: &VirtualSound<C>,
    target_volume: f32,
    budget: &VirtualVoiceBudget<C>,
    router: &QueuePoolRouter<C>,
) {
    let mut player = SamplePlayer::new(sound.handle.clone());
    if sound.looping {
        player = player.looping();
    }

    let mut ec = commands.entity(entity);
    ec.insert((
        player,
        sound.category,
        BaseVolume(sound.base_volume),
        BasePitch::default(),
        // `OnComplete::Remove` keeps the entity when seedling ends the voice
        // (steal, queue expiry, one-shot completion) so `reclaim_lost_voices`
        // can apply the queue's own policy.
        PlaybackSettings {
            on_complete: OnComplete::Remove,
            ..Default::default()
        },
        SamplePriority(budget.sample_priority),
        sample_effects![VolumeNode::from_linear(0.0)],
        FadeInAudio::new(budget.crossfade, target_volume),
        Audible,
        Name::new(format!("{:?}", sound.category)),
    ));
    (router.route)(&mut ec, sound);
    if let Some(parent) = sound.parent {
        ec.insert((ChildOf(parent), Transform::default()));
    } else if let Some(position) = sound.position {
        ec.insert(Transform::from_translation(position.as_vec3()));
    }
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

/// Returns a demoted looping entry to the virtual state once its fade-out
/// completes ([`FadeOutAudio`] removes itself, leaving `Retiring` behind).
fn finish_demotions<C: AudioCategory>(
    mut commands: Commands,
    router: Res<QueuePoolRouter<C>>,
    finished: Query<
        (Entity, Has<Sampler>),
        (With<VirtualSound<C>>, With<Retiring>, Without<FadeOutAudio>),
    >,
) {
    for (entity, has_sampler) in &finished {
        release_voice::<C>(&mut commands, entity, has_sampler, &router);
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
            release_voice::<C>(&mut commands, entity, has_sampler, &router);
        } else {
            commands.entity(entity).despawn();
        }
    }
}

/// Strips everything a promotion added — the bare `C` component, the
/// per-sound baselines, and (through the router's release hook) the pool
/// label — leaving a bare [`VirtualSound`] entry eligible for re-promotion.
/// The entry's authored volume survives in `VirtualSound::base_volume`, so
/// the next promotion restores it.
fn release_voice<C: AudioCategory>(
    commands: &mut Commands,
    entity: Entity,
    has_sampler: bool,
    router: &QueuePoolRouter<C>,
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
    (router.release)(&mut ec);
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

    fn queue_app(budget: VirtualVoiceBudget<TestSound>) -> App {
        let mut app = physics_app();
        app.add_plugins(VirtualVoiceQueuePlugin::new().with_budget(budget));
        app
    }

    fn handle(id: u128) -> Handle<AudioSample> {
        Handle::Uuid(
            bevy::asset::uuid::Uuid::from_u128(id),
            core::marker::PhantomData,
        )
    }

    fn eid(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("test id is a valid entity index")
    }

    fn entry(id: u32, significance: f32, audible: bool) -> SignificanceEntry {
        SignificanceEntry {
            entity: eid(id),
            significance,
            audible,
        }
    }

    #[test]
    fn top_n_by_significance_are_promoted() {
        let entries = [
            entry(0, 1.0, false),
            entry(1, 0.5, false),
            entry(2, 0.8, false),
        ];
        let decisions: HashMap<_, _> = rank_by_significance(&entries, 2, 0).into_iter().collect();

        assert_eq!(decisions[&eid(0)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(2)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Hold);
    }

    #[test]
    fn a_more_significant_newcomer_demotes_the_weakest_audible() {
        let entries = [
            entry(0, 0.9, true),  // audible, mid
            entry(1, 0.2, true),  // audible, weakest
            entry(2, 1.0, false), // new, most significant
        ];
        let decisions: HashMap<_, _> = rank_by_significance(&entries, 2, 0).into_iter().collect();

        assert_eq!(decisions[&eid(2)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(0)], VoiceDecision::Hold);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Demote);
    }

    #[test]
    fn a_less_significant_newcomer_does_not_disturb_playing_voices() {
        let entries = [
            entry(0, 0.9, true),
            entry(1, 0.7, true),
            entry(2, 0.1, false), // new, weakest
        ];
        let decisions: HashMap<_, _> = rank_by_significance(&entries, 2, 0).into_iter().collect();

        assert_eq!(decisions[&eid(0)], VoiceDecision::Hold);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Hold);
        assert_eq!(decisions[&eid(2)], VoiceDecision::Hold);
    }

    #[test]
    fn retiring_voices_reserve_their_slot_until_gone() {
        // Budget of 2, but one voice is already retiring (mid fade-out) —
        // only one new promotion should fit until it's gone.
        let entries = [entry(0, 1.0, false), entry(1, 0.9, false)];
        let decisions: HashMap<_, _> = rank_by_significance(&entries, 2, 1).into_iter().collect();

        assert_eq!(decisions[&eid(0)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Hold);
    }

    #[test]
    fn retiring_voices_never_evict_audible_holds() {
        // Budget 2: A=1.0 and B=0.9 audible when a louder C=1.1 arrives.
        // Frame 1: C displaces B (the weakest), crossfading past it.
        let decisions: HashMap<_, _> = rank_by_significance(
            &[
                entry(0, 1.0, true),
                entry(1, 0.9, true),
                entry(2, 1.1, false),
            ],
            2,
            0,
        )
        .into_iter()
        .collect();
        assert_eq!(decisions[&eid(0)], VoiceDecision::Hold);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Demote);
        assert_eq!(decisions[&eid(2)], VoiceDecision::Promote);

        // Frames 2+: B is retiring (excluded from entries). However many
        // retiring voices linger mid-fade, A and C keep their voices — the
        // reservation throttles promotions only, never evicts audible holds.
        for retiring in 1..=2 {
            let decisions: HashMap<_, _> =
                rank_by_significance(&[entry(0, 1.0, true), entry(2, 1.1, true)], 2, retiring)
                    .into_iter()
                    .collect();
            assert_eq!(
                decisions[&eid(0)],
                VoiceDecision::Hold,
                "retiring={retiring}"
            );
            assert_eq!(
                decisions[&eid(2)],
                VoiceDecision::Hold,
                "retiring={retiring}"
            );
        }
    }

    #[test]
    fn ties_resolve_by_input_order() {
        let entries = [
            entry(0, 0.5, false),
            entry(1, 0.5, false),
            entry(2, 0.5, false),
        ];
        let decisions: HashMap<_, _> = rank_by_significance(&entries, 2, 0).into_iter().collect();

        assert_eq!(decisions[&eid(0)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(2)], VoiceDecision::Hold);
    }

    #[test]
    fn zero_budget_holds_or_demotes_everything() {
        let entries = [entry(0, 1.0, true), entry(1, 0.5, false)];
        let decisions: HashMap<_, _> = rank_by_significance(&entries, 0, 0).into_iter().collect();

        assert_eq!(decisions[&eid(0)], VoiceDecision::Demote);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Hold);
    }

    #[test]
    fn nan_significance_does_not_panic() {
        // `total_cmp` places NaN above every finite value; the plugin's
        // enqueue path sanitizes weights so NaN never reaches ranking.
        let entries = [entry(0, f32::NAN, false), entry(1, 0.5, false)];
        let decisions: HashMap<_, _> = rank_by_significance(&entries, 1, 0).into_iter().collect();

        assert_eq!(decisions[&eid(0)], VoiceDecision::Promote);
        assert_eq!(decisions[&eid(1)], VoiceDecision::Hold);
    }

    #[test]
    fn decisions_are_returned_in_input_order() {
        let entries = [
            entry(7, 0.1, false),
            entry(3, 0.9, false),
            entry(5, 0.5, false),
        ];
        let decisions = rank_by_significance(&entries, 1, 0);

        let order: Vec<Entity> = decisions.iter().map(|(entity, _)| *entity).collect();
        assert_eq!(order, vec![eid(7), eid(3), eid(5)]);
    }

    #[test]
    fn budget_builder_sets_all_fields() {
        let budget = VirtualVoiceBudget::<TestSound>::new(8)
            .with_crossfade(Duration::from_millis(30))
            .with_max_wait(Duration::from_millis(200))
            .with_sample_priority(5)
            .with_displacement_margin(2.0);

        assert_eq!(budget.max_audible, 8);
        assert_eq!(budget.crossfade, Duration::from_millis(30));
        assert_eq!(budget.max_wait, Duration::from_millis(200));
        assert_eq!(budget.sample_priority, 5);
        assert_eq!(budget.displacement_margin, 2.0);
    }

    #[test]
    fn budget_builder_sanitizes_the_displacement_margin() {
        let below_one = VirtualVoiceBudget::<TestSound>::new(1).with_displacement_margin(0.5);
        assert_eq!(below_one.displacement_margin, 1.0);

        let non_finite = VirtualVoiceBudget::<TestSound>::new(1).with_displacement_margin(f32::NAN);
        assert_eq!(non_finite.displacement_margin, 1.0);
    }

    #[test]
    fn play_queued_audio_defaults() {
        let msg = PlayQueuedAudio::new(Handle::default(), TestSound::Sfx);
        assert!((msg.volume - 1.0).abs() < f32::EPSILON);
        assert!((msg.priority - 1.0).abs() < f32::EPSILON);
        assert!(!msg.looping);

        let msg = msg.with_priority(2.5).with_volume(0.3).looping();
        assert!((msg.priority - 2.5).abs() < f32::EPSILON);
        assert!((msg.volume - 0.3).abs() < f32::EPSILON);
        assert!(msg.looping);
    }

    #[test]
    fn enqueue_sanitizes_non_finite_and_negative_weights() {
        let mut app = queue_app(VirtualVoiceBudget::new(0));
        app.world_mut().write_message(
            PlayQueuedAudio::new(Handle::default(), TestSound::Sfx)
                .with_volume(f32::NAN)
                .with_priority(-3.0),
        );
        app.update_n(1);

        let world = app.world_mut();
        let sound = world
            .query::<&VirtualSound<TestSound>>()
            .single(world)
            .expect("entry spawned");
        assert_eq!(sound.base_volume, 0.0);
        assert_eq!(sound.priority, 0.0);
    }

    #[test]
    fn promotion_carries_elevated_sample_priority_and_name() {
        let mut app = queue_app(VirtualVoiceBudget::new(1).with_sample_priority(3));
        app.world_mut()
            .write_message(PlayQueuedAudio::new(Handle::default(), TestSound::Sfx).looping());
        app.update_n(1);

        let world = app.world_mut();
        let (priority, settings) = world
            .query_filtered::<(&SamplePriority, &PlaybackSettings), With<Audible>>()
            .single(world)
            .expect("promoted voice");
        assert_eq!(priority.0, 3);
        assert!(matches!(settings.on_complete, OnComplete::Remove));
        assert!(
            world
                .query_filtered::<&Name, With<Audible>>()
                .single(world)
                .is_ok()
        );
    }

    #[test]
    fn demoted_looping_entry_returns_to_virtual() {
        let mut app = queue_app(VirtualVoiceBudget::new(1).with_crossfade(Duration::ZERO));
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(1), TestSound::Sfx)
                .looping()
                .with_volume(0.5),
        );
        app.update_n(1);

        let world = app.world_mut();
        let quiet = world
            .query_filtered::<Entity, With<VirtualSound<TestSound>>>()
            .single(world)
            .expect("first entry promoted");
        assert!(app.world().get::<Audible>(quiet).is_some());

        // A louder loop displaces it.
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(2), TestSound::Sfx).looping());
        app.update_n(1);
        assert!(app.world().get::<Audible>(quiet).is_none());
        assert!(app.world().get::<Retiring>(quiet).is_some());
        let fade = app.world().get::<FadeOutAudio>(quiet).expect("fading out");
        assert!(!fade.despawn_on_complete);
        // The retiring fader drops to default priority so pool pressure
        // steals it before any live promoted voice.
        assert_eq!(app.world().get::<SamplePriority>(quiet).unwrap().0, 0);

        // Effects never resolve in tests, so completion waits out the
        // unresolved-effects grace; step well past it.
        app.update_n(60);
        assert!(app.world().get_entity(quiet).is_ok(), "loop entry survives");
        assert!(app.world().get::<Retiring>(quiet).is_none());
        assert!(app.world().get::<Audible>(quiet).is_none());
        assert!(app.world().get::<SamplePlayer>(quiet).is_none());
        assert!(app.world().get::<VirtualSound<TestSound>>(quiet).is_some());
    }

    #[test]
    fn demoted_one_shot_despawns_after_its_fade() {
        let mut app = queue_app(VirtualVoiceBudget::new(1).with_crossfade(Duration::ZERO));
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(1), TestSound::Sfx)
                .looping()
                .with_volume(0.5),
        );
        app.update_n(1);
        // Swap looping/one-shot roles: demote a one-shot instead.
        let world = app.world_mut();
        let mut sound = world
            .query::<&mut VirtualSound<TestSound>>()
            .single_mut(world)
            .expect("entry exists");
        sound.looping = false;
        let one_shot = {
            let world = app.world_mut();
            world
                .query_filtered::<Entity, With<VirtualSound<TestSound>>>()
                .single(world)
                .expect("entry exists")
        };

        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(2), TestSound::Sfx).looping());
        app.update_n(60);
        assert!(
            app.world().get_entity(one_shot).is_err(),
            "one-shot dropped"
        );
    }

    #[test]
    fn released_loop_is_repromoted_when_a_slot_frees_up() {
        let mut app = queue_app(VirtualVoiceBudget::new(1).with_crossfade(Duration::ZERO));
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(1), TestSound::Sfx)
                .looping()
                .with_volume(0.5),
        );
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(2), TestSound::Sfx).looping());
        app.update_n(1);

        let world = app.world_mut();
        let quiet: Vec<Entity> = world
            .query_filtered::<Entity, (With<VirtualSound<TestSound>>, Without<Audible>)>()
            .iter(world)
            .collect();
        assert_eq!(quiet.len(), 1, "quieter loop waits as virtual");
        let quiet = quiet[0];

        // Stop the loud loop; once its fade-out completes and its retiring
        // slot clears, the waiting loop is promoted.
        app.world_mut()
            .write_message(StopQueuedAudio::<TestSound>::all().with_handle(handle(2)));
        app.update_n(60);
        assert!(app.world().get::<Audible>(quiet).is_some());
    }

    #[test]
    fn reclaim_applies_policy_to_voices_seedling_ended() {
        // Budget 0 so a reclaimed loop is not immediately re-promoted.
        let mut app = queue_app(VirtualVoiceBudget::new(0));
        // Simulate `OnComplete::Remove` aftermath: Audible without a
        // `SamplePlayer`.
        let looping = app
            .world_mut()
            .spawn((
                VirtualSound {
                    handle: Handle::default(),
                    category: TestSound::Sfx,
                    looping: true,
                    parent: None,
                    position: None,
                    base_volume: 1.0,
                    priority: 1.0,
                    requested_at: Duration::ZERO,
                },
                Audible,
            ))
            .id();
        let one_shot = app
            .world_mut()
            .spawn((
                VirtualSound {
                    handle: Handle::default(),
                    category: TestSound::Sfx,
                    looping: false,
                    parent: None,
                    position: None,
                    base_volume: 1.0,
                    priority: 1.0,
                    requested_at: Duration::ZERO,
                },
                Audible,
            ))
            .id();

        app.update_n(1);
        assert!(
            app.world().get_entity(one_shot).is_err(),
            "one-shot dropped"
        );
        assert!(app.world().get_entity(looping).is_ok(), "loop kept");
        assert!(app.world().get::<Audible>(looping).is_none());
    }

    #[test]
    fn retiring_entries_of_another_category_type_do_not_shrink_the_budget() {
        #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
        #[reflect(Component)]
        enum OtherSound {
            #[default]
            Music,
        }
        impl AudioCategory for OtherSound {
            type Config = TestConfig;
            fn volume(&self, _config: &Self::Config) -> f32 {
                1.0
            }
        }

        let mut app = queue_app(VirtualVoiceBudget::new(1));
        app.add_plugins(
            VirtualVoiceQueuePlugin::<OtherSound>::new().with_budget(VirtualVoiceBudget::new(1)),
        );

        // A retiring entry in the *other* category's queue.
        app.world_mut().spawn((
            VirtualSound {
                handle: Handle::default(),
                category: OtherSound::Music,
                looping: true,
                parent: None,
                position: None,
                base_volume: 1.0,
                priority: 1.0,
                requested_at: Duration::ZERO,
            },
            Retiring,
            FadeOutAudio::new(Duration::from_secs(10)).keep_entity(),
        ));

        app.world_mut()
            .write_message(PlayQueuedAudio::new(Handle::default(), TestSound::Sfx).looping());
        app.update_n(1);

        let world = app.world_mut();
        assert!(
            world
                .query_filtered::<(), (With<VirtualSound<TestSound>>, With<Audible>)>()
                .single(world)
                .is_ok(),
            "TestSound promotion unaffected by OtherSound's retiring voice"
        );
    }

    #[test]
    fn config_volume_changes_rerank_waiting_and_audible_entries() {
        let mut app = queue_app(VirtualVoiceBudget::new(1).with_crossfade(Duration::ZERO));
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(1), TestSound::Sfx).looping());
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(2), TestSound::Ambience)
                .looping()
                .with_volume(0.5),
        );
        app.update_n(1);

        let world = app.world_mut();
        let (audible_first, _) = world
            .query_filtered::<(Entity, &VirtualSound<TestSound>), With<Audible>>()
            .single(world)
            .expect("one audible voice");
        assert_eq!(
            app.world()
                .get::<VirtualSound<TestSound>>(audible_first)
                .unwrap()
                .category(),
            TestSound::Sfx
        );

        // Boost ambience volume: the waiting entry now outranks the SFX one.
        app.world_mut().resource_mut::<TestConfig>().ambience = 10.0;
        app.update_n(1);

        let world = app.world_mut();
        let audible: Vec<TestSound> = world
            .query_filtered::<&VirtualSound<TestSound>, With<Audible>>()
            .iter(world)
            .map(VirtualSound::category)
            .collect();
        assert_eq!(audible, vec![TestSound::Ambience]);
        assert!(app.world().get::<Retiring>(audible_first).is_some());
    }

    #[test]
    fn loops_are_exempt_from_max_wait() {
        let mut app =
            queue_app(VirtualVoiceBudget::new(0).with_max_wait(Duration::from_millis(50)));
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(1), TestSound::Sfx).looping());
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(2), TestSound::Sfx));
        app.update_n(10);

        let world = app.world_mut();
        let remaining: Vec<bool> = world
            .query::<&VirtualSound<TestSound>>()
            .iter(world)
            .map(VirtualSound::looping)
            .collect();
        assert_eq!(remaining, vec![true], "one-shot dropped, loop waits on");
    }

    #[test]
    fn entries_with_a_dead_parent_are_dropped() {
        let mut app = queue_app(VirtualVoiceBudget::new(1));
        let parent = app.world_mut().spawn_empty().id();
        app.world_mut().write_message(
            PlayQueuedAudio::new(Handle::default(), TestSound::Sfx)
                .looping()
                .with_parent(parent),
        );
        app.world_mut().despawn(parent);
        app.update_n(1);

        let world = app.world_mut();
        assert_eq!(
            world
                .query::<&VirtualSound<TestSound>>()
                .iter(world)
                .count(),
            0
        );
    }

    #[test]
    fn stop_queued_audio_despawns_virtual_and_fades_audible() {
        let mut app = queue_app(VirtualVoiceBudget::new(1));
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(1), TestSound::Sfx).looping());
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(2), TestSound::Sfx)
                .looping()
                .with_volume(0.5),
        );
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
        let world = app.world_mut();
        assert_eq!(
            world
                .query::<&VirtualSound<TestSound>>()
                .iter(world)
                .count(),
            0
        );
    }

    #[test]
    fn newcomer_inside_the_displacement_margin_does_not_displace() {
        let mut app = queue_app(VirtualVoiceBudget::new(1));
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(1), TestSound::Sfx).looping());
        app.update_n(1);

        let world = app.world_mut();
        let incumbent = world
            .query_filtered::<Entity, With<Audible>>()
            .single(world)
            .expect("incumbent promoted");

        // 10% louder: inside the default 1.25 margin, so no churn.
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(2), TestSound::Sfx)
                .looping()
                .with_volume(1.1),
        );
        app.update_n(2);

        assert!(app.world().get::<Audible>(incumbent).is_some());
        assert!(app.world().get::<Retiring>(incumbent).is_none());
    }

    #[test]
    fn newcomer_beyond_the_displacement_margin_displaces() {
        let mut app = queue_app(VirtualVoiceBudget::new(1));
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(1), TestSound::Sfx).looping());
        app.update_n(1);

        let world = app.world_mut();
        let incumbent = world
            .query_filtered::<Entity, With<Audible>>()
            .single(world)
            .expect("incumbent promoted");

        // 30% louder: beats the default 1.25 margin.
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(2), TestSound::Sfx)
                .looping()
                .with_volume(1.3),
        );
        app.update_n(1);

        assert!(app.world().get::<Audible>(incumbent).is_none());
        assert!(app.world().get::<Retiring>(incumbent).is_some());
    }

    #[test]
    fn nan_config_volume_is_sanitized_before_ranking() {
        let mut app = queue_app(VirtualVoiceBudget::new(1));
        app.world_mut().resource_mut::<TestConfig>().sfx = f32::NAN;
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(1), TestSound::Sfx).looping());
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(2), TestSound::Ambience)
                .looping()
                .with_volume(0.5),
        );
        app.update_n(1);

        // NaN significance would outrank everything under `total_cmp`; the
        // sanitized recompute ranks the NaN-config category at zero instead.
        let world = app.world_mut();
        let audible: Vec<TestSound> = world
            .query_filtered::<&VirtualSound<TestSound>, With<Audible>>()
            .iter(world)
            .map(VirtualSound::category)
            .collect();
        assert_eq!(audible, vec![TestSound::Ambience]);
    }

    #[test]
    fn stop_around_fade_completion_always_despawns() {
        // A stop arriving on the exact frame a keep-entity demotion fade
        // completes must not be lost to the fade's own self-removal —
        // sweep stop delivery across the completion window to cover it.
        for stop_after in (0..=40).step_by(2) {
            let mut app = queue_app(VirtualVoiceBudget::new(1).with_crossfade(Duration::ZERO));
            app.world_mut()
                .write_message(PlayQueuedAudio::new(handle(1), TestSound::Sfx).looping());
            app.update_n(1);
            // Loud enough to beat the displacement margin and demote the
            // incumbent into its keep-entity retiring fade.
            app.world_mut().write_message(
                PlayQueuedAudio::new(handle(2), TestSound::Sfx)
                    .looping()
                    .with_volume(2.0),
            );
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

    // ==================== Admission controls ====================

    fn entry_count(app: &mut App) -> usize {
        let world = app.world_mut();
        world
            .query::<&VirtualSound<TestSound>>()
            .iter(world)
            .count()
    }

    #[test]
    fn global_per_frame_budget_caps_admissions() {
        let mut app = queue_app(VirtualVoiceBudget::new(0).with_max_admissions_per_frame(1));
        for id in 1..=3 {
            app.world_mut()
                .write_message(PlayQueuedAudio::new(handle(id), TestSound::Sfx).looping());
        }
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 1, "two of three dropped this frame");

        // The cap is per frame, not cumulative.
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(4), TestSound::Sfx).looping());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 2);
    }

    #[test]
    fn per_sound_per_frame_cap_only_limits_that_sample() {
        let mut app = queue_app(VirtualVoiceBudget::new(0));
        for _ in 0..3 {
            app.world_mut().write_message(
                PlayQueuedAudio::new(handle(1), TestSound::Sfx)
                    .looping()
                    .with_max_per_frame(1),
            );
        }
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(2), TestSound::Sfx).looping());
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
        let mut app = queue_app(VirtualVoiceBudget::new(0));
        let request = || {
            PlayQueuedAudio::new(handle(1), TestSound::Sfx)
                .looping()
                .with_max_concurrent(1)
        };

        // Two same-frame requests: the second already sees the first.
        app.world_mut().write_message(request());
        app.world_mut().write_message(request());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 1);

        // A later request still sees the live entry.
        app.world_mut().write_message(request());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 1);

        // Once the entry is gone, the next request is admitted again.
        app.world_mut()
            .write_message(StopQueuedAudio::<TestSound>::all());
        app.update_n(1);
        app.world_mut().write_message(request());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 1);
    }

    #[test]
    fn min_repeat_interval_rejects_rapid_repeats() {
        let mut app = queue_app(VirtualVoiceBudget::new(0));
        let request = || {
            PlayQueuedAudio::new(handle(1), TestSound::Sfx)
                .looping()
                .with_min_repeat_interval(Duration::from_millis(100))
        };

        app.world_mut().write_message(request());
        app.update_n(1);
        // Immediately re-requested: inside the interval, dropped.
        app.world_mut().write_message(request());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 1);

        // Past the interval (fixed steps are ~15.6ms), admitted again.
        app.update_n(8);
        app.world_mut().write_message(request());
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 2);
    }

    #[test]
    fn max_distance_culls_far_requests_when_a_listener_exists() {
        let mut app = queue_app(VirtualVoiceBudget::new(0));
        app.world_mut().spawn((
            SpatialListener2D,
            Transform::default(),
            GlobalTransform::default(),
        ));

        let request = |id: u128, position: Vec2| {
            PlayQueuedAudio::new(handle(id), TestSound::Sfx)
                .looping()
                .at(position)
                .with_max_distance(100.0)
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
        let mut app = queue_app(VirtualVoiceBudget::new(0));

        // No listener in the world: nothing to measure from, nothing culled.
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(1), TestSound::Sfx)
                .looping()
                .at(Vec2::new(5000.0, 0.0))
                .with_max_distance(10.0),
        );
        // A positionless request has nowhere to be measured from either.
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(2), TestSound::Sfx)
                .looping()
                .with_max_distance(10.0),
        );
        app.update_n(1);
        assert_eq!(entry_count(&mut app), 2);
    }

    #[test]
    fn with_max_distance_sanitizes_hostile_values() {
        let nan = PlayQueuedAudio::new(handle(1), TestSound::Sfx).with_max_distance(f32::NAN);
        assert_eq!(nan.max_distance, None, "a non-finite distance never culls");

        let negative = PlayQueuedAudio::new(handle(1), TestSound::Sfx).with_max_distance(-5.0);
        assert_eq!(negative.max_distance, Some(0.0));
    }

    fn recorded_admissions(app: &App) -> usize {
        app.world()
            .resource::<AdmissionState<TestSound>>()
            .last_admitted
            .len()
    }

    #[test]
    fn admission_bookkeeping_stays_bounded() {
        let mut app = queue_app(VirtualVoiceBudget::new(0));

        // Interval-less admissions are not recorded at all.
        for id in 1..=8 {
            app.world_mut()
                .write_message(PlayQueuedAudio::new(handle(id), TestSound::Sfx).looping());
        }
        app.update_n(1);
        assert_eq!(recorded_admissions(&app), 0);

        // Recorded admissions past the prune threshold shed every entry too
        // old to block a request again.
        for id in 1..=80 {
            app.world_mut().write_message(
                PlayQueuedAudio::new(handle(id), TestSound::Sfx)
                    .looping()
                    .with_min_repeat_interval(Duration::from_millis(1)),
            );
        }
        app.update_n(1);
        assert_eq!(recorded_admissions(&app), 80);

        // A frame later everything recorded is older than its interval; the
        // next recorded admission prunes the lot.
        app.update_n(1);
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(1000), TestSound::Sfx)
                .looping()
                .with_min_repeat_interval(Duration::from_millis(1)),
        );
        app.update_n(1);
        assert_eq!(recorded_admissions(&app), 1);
    }

    #[test]
    fn a_config_change_keeps_a_promoted_entry_s_own_volume() {
        let mut app = queue_app(VirtualVoiceBudget::new(1));
        app.add_plugins(crate::MsgSeedlingPlugin::<TestSound>::default());
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(1), TestSound::Sfx)
                .looping()
                .with_volume(0.25),
        );
        app.update_n(1);

        let world = app.world_mut();
        let voice = world
            .query_filtered::<Entity, With<Audible>>()
            .single(world)
            .expect("promoted voice");
        assert_eq!(
            app.world().get::<BaseVolume>(voice).copied(),
            Some(BaseVolume(0.25)),
            "promotion records the entry's own level"
        );

        // Stand in for the promotion fade completing (see the ducking test).
        app.world_mut().entity_mut(voice).remove::<FadeInAudio>();
        let effect = app.world().get::<SampleEffects>(voice).expect("effects")[0];

        // `MsgSeedlingPlugin` now reaches this voice through its bare `C`.
        app.world_mut().resource_mut::<TestConfig>().sfx = 0.8;
        app.update_n(1);

        let volume = app
            .world()
            .get::<VolumeNode>(effect)
            .expect("volume node")
            .volume
            .linear();
        assert!(
            (volume - 0.2).abs() < 1e-6,
            "a settings change scales the entry's `with_volume`, it does not \
             flatten every promoted voice to the category level, got {volume}"
        );
    }

    #[test]
    fn a_released_voice_sheds_its_baselines() {
        let mut app = queue_app(VirtualVoiceBudget::new(1));
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(1), TestSound::Sfx)
                .looping()
                .with_volume(0.25),
        );
        app.update_n(1);
        let world = app.world_mut();
        let voice = world
            .query_filtered::<Entity, With<Audible>>()
            .single(world)
            .expect("promoted voice");

        // Flushed without another update: a further frame would simply
        // re-promote the entry (it is still the only one, and the budget has
        // room), putting the baselines straight back.
        {
            let router = QueuePoolRouter::<TestSound>::default();
            let world = app.world_mut();
            release_voice::<TestSound>(&mut world.commands(), voice, false, &router);
            world.flush();
        }

        assert!(app.world().get::<BaseVolume>(voice).is_none());
        assert!(app.world().get::<BasePitch>(voice).is_none());
        assert!(
            (app.world()
                .get::<VirtualSound<TestSound>>(voice)
                .expect("entry survives")
                .base_volume
                - 0.25)
                .abs()
                < 1e-6,
            "the entry's authored volume survives for the next promotion"
        );
    }

    #[test]
    fn promoted_voices_are_reached_by_ducking() {
        use crate::damping::DampingPlugin;
        use crate::ducking::{DuckingEnvelope, Ducks};

        let mut app = queue_app(VirtualVoiceBudget::new(1));
        app.add_plugins(DampingPlugin::<TestSound>::default());
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(1), TestSound::Sfx).looping());
        app.update_n(1);

        let world = app.world_mut();
        let voice = world
            .query_filtered::<Entity, With<Audible>>()
            .single(world)
            .expect("promoted voice");
        assert!(
            app.world().get::<TestSound>(voice).is_some(),
            "promotion inserts the bare category component"
        );

        // Stand in for the promotion fade completing — without an audio
        // context it never resolves — by removing its marker and settling
        // the node at the fade's target.
        app.world_mut().entity_mut(voice).remove::<FadeInAudio>();
        let effect = app.world().get::<SampleEffects>(voice).expect("effects")[0];
        app.world_mut()
            .get_mut::<VolumeNode>(effect)
            .expect("volume node")
            .volume = Volume::Linear(1.0);

        app.world_mut().entity_mut(voice).insert(Ducks);
        app.world_mut().resource_mut::<DuckingEnvelope>().trigger();
        app.update_n(5);

        let ducked_gain = app.world().resource::<DuckingEnvelope>().ducked_gain;
        let volume = app
            .world()
            .get::<VolumeNode>(effect)
            .expect("volume node")
            .volume
            .linear();
        assert!(
            (volume - ducked_gain).abs() < 1e-6,
            "the promoted voice ducks with the envelope, got {volume}"
        );
    }

    #[test]
    fn stop_queued_audio_handle_filter_stops_only_matching_entries() {
        let mut app = queue_app(VirtualVoiceBudget::new(0));
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(1), TestSound::Sfx).looping());
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(2), TestSound::Sfx).looping());
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
    fn gate_per_frame_cap_is_per_sample() {
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

    #[test]
    fn admission_request_mirrors_the_message_controls() {
        let msg = PlayQueuedAudio::new(handle(1), TestSound::Sfx)
            .at(Vec2::new(3.0, 4.0))
            .with_max_concurrent(4)
            .with_max_per_frame(2)
            .with_min_repeat_interval(Duration::from_millis(40))
            .with_max_distance(1200.0);
        let request = msg.admission_request();

        assert_eq!(request.sample, handle(1).id());
        assert_eq!(request.position, Some(Vec2::new(3.0, 4.0)));
        assert_eq!(request.max_concurrent, Some(4));
        assert_eq!(request.max_per_frame, Some(2));
        assert_eq!(request.min_repeat_interval, Some(Duration::from_millis(40)));
        assert_eq!(request.max_distance, Some(1200.0));
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

    // ==================== Pool routing ====================

    #[test]
    fn default_routing_splits_spatial_and_non_spatial() {
        let mut app = queue_app(VirtualVoiceBudget::new(2));
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(1), TestSound::Sfx).looping());
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(2), TestSound::Sfx)
                .looping()
                .at(Vec2::new(1.0, 2.0)),
        );
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
            if sound.position().is_some() {
                assert!(spatial_pool && !default_pool, "positioned voice");
            } else {
                assert!(default_pool && !spatial_pool, "positionless voice");
            }
        }
        assert_eq!(promoted, 2);
    }

    #[test]
    fn a_custom_pool_router_replaces_promotion_routing_and_release() {
        #[derive(Component)]
        struct CustomPool;

        let mut app = physics_app();
        app.add_plugins(
            VirtualVoiceQueuePlugin::<TestSound>::new()
                .with_budget(VirtualVoiceBudget::new(1).with_crossfade(Duration::ZERO))
                .with_pool_router(QueuePoolRouter::new(
                    |ec: &mut EntityCommands, _sound: &VirtualSound<TestSound>| {
                        ec.insert(CustomPool);
                    },
                    |ec: &mut EntityCommands| {
                        ec.remove::<CustomPool>();
                    },
                )),
        );
        app.world_mut().write_message(
            PlayQueuedAudio::new(handle(1), TestSound::Sfx)
                .looping()
                .with_volume(0.5),
        );
        app.update_n(1);

        let world = app.world_mut();
        let voice = world
            .query_filtered::<Entity, With<Audible>>()
            .single(world)
            .expect("promoted voice");
        assert!(app.world().get::<CustomPool>(voice).is_some());
        assert!(app.world().get::<DefaultPool>(voice).is_none());
        assert!(app.world().get::<SpatialPool>(voice).is_none());

        // Displace it; once the demotion fade completes, the release hook
        // strips the custom label along with the rest of the voice.
        app.world_mut()
            .write_message(PlayQueuedAudio::new(handle(2), TestSound::Sfx).looping());
        app.update_n(60);

        assert!(
            app.world().get_entity(voice).is_ok(),
            "loop returns to virtual"
        );
        assert!(app.world().get::<Audible>(voice).is_none());
        assert!(app.world().get::<CustomPool>(voice).is_none());
    }
}
