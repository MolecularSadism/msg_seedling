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
//! This queue is independent of [`PlayAudio`](crate::PlayAudio),
//! [`StopAudio`](crate::StopAudio), [`FadeAudio`](crate::FadeAudio), and the
//! plugin's per-category volume-update systems — a promoted entry does not
//! carry the bare `C` component those systems match on, so it is stopped and
//! faded through [`StopQueuedAudio`] instead. Significance tracks the
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

use core::time::Duration;

use bevy::ecs::entity::Entities;
use bevy::prelude::*;
use bevy_seedling::pool::label::PoolLabelContainer;
use bevy_seedling::pool::{CompletionReason, Sampler};
use bevy_seedling::prelude::*;
use bevy_seedling::sample::{AudioSample, QueuedSample};

use crate::fade::{FadeInAudio, FadeOutAudio};
use crate::messages::SpatialPosition;
use crate::traits::AudioCategory;

const DEFAULT_MAX_AUDIBLE: usize = 16;
const DEFAULT_CROSSFADE: Duration = Duration::from_millis(50);
const DEFAULT_MAX_WAIT: Duration = Duration::from_millis(500);
const DEFAULT_SAMPLE_PRIORITY: i32 = 2;
const DEFAULT_DISPLACEMENT_MARGIN: f32 = 1.25;

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
}

impl<C: AudioCategory> Default for VirtualVoiceQueuePlugin<C> {
    fn default() -> Self {
        Self {
            budget: VirtualVoiceBudget::default(),
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
}

impl<C: AudioCategory> Plugin for VirtualVoiceQueuePlugin<C> {
    fn build(&self, app: &mut App) {
        crate::fade::plugin(app);

        app.register_type::<VirtualSound<C>>();
        app.register_type::<Audible>();
        app.register_type::<Retiring>();
        app.init_resource::<C::Config>();
        app.insert_resource(self.budget.clone());
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
                .chain(),
        );
    }
}

fn enqueue_queued_audio<C: AudioCategory>(
    mut commands: Commands,
    time: Res<Time>,
    mut messages: MessageReader<PlayQueuedAudio<C>>,
) {
    for msg in messages.read() {
        commands.spawn(VirtualSound {
            handle: msg.handle.clone(),
            category: msg.category,
            looping: msg.looping,
            parent: msg.parent,
            position: msg.position,
            base_volume: sanitize_weight(msg.volume),
            priority: sanitize_weight(msg.priority),
            requested_at: time.elapsed(),
        });
    }
}

/// Non-finite → `0.0`, otherwise clamped to `>= 0.0`, keeping significance
/// math and ranking total-order safe.
fn sanitize_weight(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
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

fn rank_virtual_voices<C: AudioCategory>(
    mut commands: Commands,
    budget: Res<VirtualVoiceBudget<C>>,
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
                promote(&mut commands, entity, sound, *target_volume, &budget);
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
/// again.
fn promote<C: AudioCategory>(
    commands: &mut Commands,
    entity: Entity,
    sound: &VirtualSound<C>,
    target_volume: f32,
    budget: &VirtualVoiceBudget<C>,
) {
    let mut player = SamplePlayer::new(sound.handle.clone());
    if sound.looping {
        player = player.looping();
    }

    let mut ec = commands.entity(entity);
    ec.insert((
        player,
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
    if sound.parent.is_some() || sound.position.is_some() {
        ec.insert(SpatialPool);
    } else {
        ec.insert(DefaultPool);
    }
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
    finished: Query<
        (Entity, Has<Sampler>),
        (With<VirtualSound<C>>, With<Retiring>, Without<FadeOutAudio>),
    >,
) {
    for (entity, has_sampler) in &finished {
        release_voice(&mut commands, entity, has_sampler);
    }
}

/// Applies queue policy to promoted entries whose seedling voice ended
/// (`OnComplete::Remove` stripped the `SamplePlayer`): looping entries
/// return to virtual for re-promotion, finished or dead one-shots despawn.
fn reclaim_lost_voices<C: AudioCategory>(
    mut commands: Commands,
    lost: Query<
        (Entity, &VirtualSound<C>, Has<Sampler>),
        (With<Audible>, Without<SamplePlayer>, Without<Retiring>),
    >,
) {
    for (entity, sound, has_sampler) in &lost {
        if sound.looping {
            release_voice(&mut commands, entity, has_sampler);
        } else {
            commands.entity(entity).despawn();
        }
    }
}

/// Strips everything a promotion added, leaving a bare [`VirtualSound`]
/// entry eligible for re-promotion.
fn release_voice(commands: &mut Commands, entity: Entity, has_sampler: bool) {
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
    commands
        .entity(entity)
        .despawn_related::<SampleEffects>()
        .remove_with_requires::<SamplePlayer>()
        .remove::<(
            Sampler,
            QueuedSample,
            AudioEvents,
            PoolLabelContainer,
            SpatialPool,
            DefaultPool,
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
}
