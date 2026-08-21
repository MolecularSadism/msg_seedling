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
//! they become the most significant thing requested, up to
//! [`VirtualVoiceBudget::max_wait`] — past that they're dropped, mirroring
//! how a real voice-limited engine gives up on a request that waited too
//! long to matter.
//!
//! # Scope
//!
//! This queue is independent of [`PlayAudio`](crate::PlayAudio),
//! [`StopAudio`](crate::StopAudio), [`FadeAudio`](crate::FadeAudio), and the
//! plugin's per-category volume-update systems — a promoted entry does not
//! carry the bare `C` component those systems match on, so it will not be
//! stopped, faded, or re-volumed by them. This is deliberate for v1: mixing
//! priority-scaled significance with an independent category-volume system
//! invites silent drift between the two. Route category-volume changes
//! through significance instead — recompute and re-enqueue if a sound's
//! target volume needs to track a config change while it waits.
//!
//! Significance does not currently factor in distance for spatial sounds —
//! bake any distance attenuation into [`PlayQueuedAudio::with_volume`] or
//! [`PlayQueuedAudio::with_priority`] before sending, if the caller can
//! compute it (e.g. from a listener position it already tracks).
//!
//! # Example
//!
//! ```rust,ignore
//! app.add_plugins(VirtualVoiceQueuePlugin::<Sound>::default());
//!
//! fn play_sounds(mut writer: MessageWriter<PlayQueuedAudio<Sound>>, assets: Res<AssetServer>) {
//!     // A quiet, low-priority ambience request: waits as virtual if the
//!     // queue is full of louder things, promoted if room frees up.
//!     writer.write(PlayQueuedAudio::new(handle.clone(), Sound::Ambience).with_volume(0.2));
//!
//!     // A loud explosion: outranks quieter voices and crossfades in,
//!     // smoothly displacing whichever currently-audible voice ranks lowest.
//!     writer.write(PlayQueuedAudio::new(explosion_handle, Sound::Sfx).with_priority(2.0));
//! }
//! ```

use core::time::Duration;
use std::collections::HashMap;

use bevy::prelude::*;
use bevy_seedling::prelude::*;
use bevy_seedling::sample::AudioSample;

use crate::fade::{FadeInAudio, FadeOutAudio};
use crate::messages::SpatialPosition;
use crate::traits::AudioCategory;

const DEFAULT_MAX_AUDIBLE: usize = 16;
const DEFAULT_CROSSFADE: Duration = Duration::from_millis(50);
const DEFAULT_MAX_WAIT: Duration = Duration::from_millis(500);

/// Budget for one category type's significance-ranked voice queue.
///
/// Scoped per `C` so unrelated category types (e.g. music vs. SFX) never
/// compete for the same slots when both use [`VirtualVoiceQueuePlugin`].
#[derive(Resource, Clone, Debug)]
pub struct VirtualVoiceBudget<C: AudioCategory> {
    /// Maximum number of entries that may hold a real `SamplePlayer` voice
    /// at once.
    pub max_audible: usize,
    /// Duration of the crossfade applied when an entry is promoted or
    /// demoted.
    pub crossfade: Duration,
    /// How long a never-promoted entry waits before it is dropped.
    pub max_wait: Duration,
    _phantom: core::marker::PhantomData<C>,
}

impl<C: AudioCategory> VirtualVoiceBudget<C> {
    /// Creates a budget for up to `max_audible` simultaneous real voices,
    /// using the default crossfade (50ms) and max wait (500ms).
    #[must_use]
    pub fn new(max_audible: usize) -> Self {
        Self {
            max_audible,
            crossfade: DEFAULT_CROSSFADE,
            max_wait: DEFAULT_MAX_WAIT,
            _phantom: core::marker::PhantomData,
        }
    }

    /// Sets the promotion/demotion crossfade duration.
    #[must_use]
    pub fn with_crossfade(mut self, crossfade: Duration) -> Self {
        self.crossfade = crossfade;
        self
    }

    /// Sets how long a never-promoted entry waits before being dropped.
    #[must_use]
    pub fn with_max_wait(mut self, max_wait: Duration) -> Self {
        self.max_wait = max_wait;
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
#[derive(Component, Clone)]
pub struct VirtualSound<C: AudioCategory> {
    handle: Handle<AudioSample>,
    category: C,
    looping: bool,
    parent: Option<Entity>,
    position: Option<SpatialPosition>,
    target_volume: f32,
    priority: f32,
    requested_at: Duration,
}

impl<C: AudioCategory> VirtualSound<C> {
    /// The category this sound was requested under.
    #[must_use]
    pub fn category(&self) -> C {
        self.category
    }
}

/// Marker: this entry currently owns a live `SamplePlayer`.
#[derive(Component)]
pub struct Audible;

/// Marker: this entry is fading out and will despawn when the fade
/// completes; excluded from ranking while in this state.
#[derive(Component)]
pub struct Retiring;

/// Message: request a sound through the significance-ranked virtual voice
/// queue instead of playing unconditionally.
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

        app.insert_resource(self.budget.clone());
        app.add_message::<PlayQueuedAudio<C>>();
        app.add_systems(
            Update,
            (
                enqueue_queued_audio::<C>,
                rank_virtual_voices::<C>.after(enqueue_queued_audio::<C>),
            ),
        );
    }
}

fn enqueue_queued_audio<C: AudioCategory>(
    mut commands: Commands,
    config: Res<C::Config>,
    time: Res<Time>,
    mut messages: MessageReader<PlayQueuedAudio<C>>,
) {
    for msg in messages.read() {
        let target_volume = msg.category.volume(&config) * msg.volume;
        commands.spawn(VirtualSound {
            handle: msg.handle.clone(),
            category: msg.category,
            looping: msg.looping,
            parent: msg.parent,
            position: msg.position,
            target_volume,
            priority: msg.priority.max(0.0),
            requested_at: time.elapsed(),
        });
    }
}

/// One queue entry's ranking inputs.
#[derive(Clone, Copy, Debug)]
struct Entry {
    entity: Entity,
    significance: f32,
    audible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Promote,
    Demote,
    Hold,
}

/// Pure ranking decision: given every non-retiring entry and however many
/// voices are already retiring (still occupying a real voice mid-fade-out),
/// decides which entries should hold a real voice.
///
/// Entries are ranked by `significance` descending; the top `max_audible`
/// (after reserving `retiring` slots for fades already in flight) are
/// [`Decision::Promote`]d if virtual or held if already audible, and the
/// rest are [`Decision::Demote`]d if audible or held (still waiting) if
/// virtual. Ties keep their input order, so equally significant requests
/// resolve deterministically by arrival order rather than by chance.
fn rank(entries: &[Entry], max_audible: usize, retiring: usize) -> Vec<(Entity, Decision)> {
    let mut ranked: Vec<&Entry> = entries.iter().collect();
    ranked.sort_by(|a, b| {
        b.significance
            .partial_cmp(&a.significance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut used = retiring;
    ranked
        .into_iter()
        .map(|entry| {
            let decision = if used < max_audible {
                used += 1;
                if entry.audible {
                    Decision::Hold
                } else {
                    Decision::Promote
                }
            } else if entry.audible {
                Decision::Demote
            } else {
                Decision::Hold
            };
            (entry.entity, decision)
        })
        .collect()
}

#[allow(clippy::type_complexity)]
fn rank_virtual_voices<C: AudioCategory>(
    mut commands: Commands,
    budget: Res<VirtualVoiceBudget<C>>,
    time: Res<Time>,
    eligible: Query<(Entity, &VirtualSound<C>, Has<Audible>), Without<Retiring>>,
    retiring: Query<(), With<Retiring>>,
) {
    let retiring_count = retiring.iter().count();

    let entries: Vec<Entry> = eligible
        .iter()
        .map(|(entity, sound, audible)| Entry {
            entity,
            significance: sound.target_volume * sound.priority,
            audible,
        })
        .collect();

    if entries.is_empty() {
        return;
    }

    let decisions: HashMap<Entity, Decision> = rank(&entries, budget.max_audible, retiring_count)
        .into_iter()
        .collect();

    let now = time.elapsed();

    for entry in &entries {
        match decisions.get(&entry.entity) {
            Some(Decision::Promote) => {
                let Ok((_, sound, _)) = eligible.get(entry.entity) else {
                    continue;
                };
                let mut player = SamplePlayer::new(sound.handle.clone());
                if sound.looping {
                    player = player.looping();
                }
                let is_spatial = sound.parent.is_some() || sound.position.is_some();

                let mut ec = commands.entity(entry.entity);
                if is_spatial {
                    ec.insert((
                        player,
                        SpatialPool,
                        sample_effects![VolumeNode::from_linear(0.0)],
                        FadeInAudio::new(budget.crossfade, sound.target_volume),
                        Audible,
                    ));
                } else {
                    ec.insert((
                        player,
                        DefaultPool,
                        sample_effects![VolumeNode::from_linear(0.0)],
                        FadeInAudio::new(budget.crossfade, sound.target_volume),
                        Audible,
                    ));
                }
                if let Some(parent) = sound.parent {
                    ec.insert((ChildOf(parent), Transform::default()));
                } else if let Some(position) = sound.position {
                    ec.insert(Transform::from_translation(position.as_vec3()));
                }
            }
            Some(Decision::Demote) => {
                commands
                    .entity(entry.entity)
                    .remove::<Audible>()
                    .insert((Retiring, FadeOutAudio::new(budget.crossfade)));
            }
            Some(Decision::Hold) | None => {
                if !entry.audible
                    && let Ok((_, sound, _)) = eligible.get(entry.entity)
                    && now.saturating_sub(sound.requested_at) > budget.max_wait
                {
                    commands.entity(entry.entity).despawn();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::AudioConfig;

    fn eid(id: u32) -> Entity {
        Entity::from_raw_u32(id).expect("test id is a valid entity index")
    }

    fn entry(id: u32, significance: f32, audible: bool) -> Entry {
        Entry {
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
        let decisions: HashMap<_, _> = rank(&entries, 2, 0).into_iter().collect();

        assert_eq!(decisions[&eid(0)], Decision::Promote);
        assert_eq!(decisions[&eid(2)], Decision::Promote);
        assert_eq!(decisions[&eid(1)], Decision::Hold);
    }

    #[test]
    fn a_more_significant_newcomer_demotes_the_weakest_audible() {
        let entries = [
            entry(0, 0.9, true),  // audible, mid
            entry(1, 0.2, true),  // audible, weakest
            entry(2, 1.0, false), // new, most significant
        ];
        let decisions: HashMap<_, _> = rank(&entries, 2, 0).into_iter().collect();

        assert_eq!(decisions[&eid(2)], Decision::Promote);
        assert_eq!(decisions[&eid(0)], Decision::Hold);
        assert_eq!(decisions[&eid(1)], Decision::Demote);
    }

    #[test]
    fn a_less_significant_newcomer_does_not_disturb_playing_voices() {
        let entries = [
            entry(0, 0.9, true),
            entry(1, 0.7, true),
            entry(2, 0.1, false), // new, weakest
        ];
        let decisions: HashMap<_, _> = rank(&entries, 2, 0).into_iter().collect();

        assert_eq!(decisions[&eid(0)], Decision::Hold);
        assert_eq!(decisions[&eid(1)], Decision::Hold);
        assert_eq!(decisions[&eid(2)], Decision::Hold);
    }

    #[test]
    fn retiring_voices_reserve_their_slot_until_gone() {
        // Budget of 2, but one voice is already retiring (mid fade-out) —
        // only one new promotion should fit until it's gone.
        let entries = [entry(0, 1.0, false), entry(1, 0.9, false)];
        let decisions: HashMap<_, _> = rank(&entries, 2, 1).into_iter().collect();

        assert_eq!(decisions[&eid(0)], Decision::Promote);
        assert_eq!(decisions[&eid(1)], Decision::Hold);
    }

    #[test]
    fn ties_resolve_by_input_order() {
        let entries = [
            entry(0, 0.5, false),
            entry(1, 0.5, false),
            entry(2, 0.5, false),
        ];
        let decisions: HashMap<_, _> = rank(&entries, 2, 0).into_iter().collect();

        assert_eq!(decisions[&eid(0)], Decision::Promote);
        assert_eq!(decisions[&eid(1)], Decision::Promote);
        assert_eq!(decisions[&eid(2)], Decision::Hold);
    }

    #[test]
    fn zero_budget_holds_or_demotes_everything() {
        let entries = [entry(0, 1.0, true), entry(1, 0.5, false)];
        let decisions: HashMap<_, _> = rank(&entries, 0, 0).into_iter().collect();

        assert_eq!(decisions[&eid(0)], Decision::Demote);
        assert_eq!(decisions[&eid(1)], Decision::Hold);
    }

    #[test]
    fn budget_builder_sets_crossfade_and_max_wait() {
        #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
        #[reflect(Component)]
        enum TestSound {
            #[default]
            Sfx,
        }
        #[derive(Resource, Clone, Default)]
        struct TestConfig;
        impl AudioConfig for TestConfig {
            fn master_volume(&self) -> f32 {
                1.0
            }
        }
        impl AudioCategory for TestSound {
            type Config = TestConfig;
            fn volume(&self, _config: &Self::Config) -> f32 {
                1.0
            }
        }

        let budget = VirtualVoiceBudget::<TestSound>::new(8)
            .with_crossfade(Duration::from_millis(30))
            .with_max_wait(Duration::from_millis(200));

        assert_eq!(budget.max_audible, 8);
        assert_eq!(budget.crossfade, Duration::from_millis(30));
        assert_eq!(budget.max_wait, Duration::from_millis(200));
    }

    #[test]
    fn play_queued_audio_defaults() {
        #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
        #[reflect(Component)]
        enum TestSound {
            #[default]
            Sfx,
        }
        #[derive(Resource, Clone, Default)]
        struct TestConfig;
        impl AudioConfig for TestConfig {
            fn master_volume(&self) -> f32 {
                1.0
            }
        }
        impl AudioCategory for TestSound {
            type Config = TestConfig;
            fn volume(&self, _config: &Self::Config) -> f32 {
                1.0
            }
        }

        let msg = PlayQueuedAudio::new(Handle::default(), TestSound::Sfx);
        assert!((msg.volume - 1.0).abs() < f32::EPSILON);
        assert!((msg.priority - 1.0).abs() < f32::EPSILON);
        assert!(!msg.looping);

        let msg = msg.with_priority(2.5).with_volume(0.3).looping();
        assert!((msg.priority - 2.5).abs() < f32::EPSILON);
        assert!((msg.volume - 0.3).abs() < f32::EPSILON);
        assert!(msg.looping);
    }
}
