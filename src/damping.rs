//! Sound damping fields: world-space volumes that muffle the sound crossing
//! them.
//!
//! A [`SoundDampingField`] is a sphere of influence placed on any entity with
//! a transform. A sound it acts on is attenuated on three axes:
//!
//! - **volume** — a linear multiplier folded into the sound's volume node, on
//!   top of the category volume pipeline and the sound's own spawn volume;
//! - **cutoff** — the frequency of the sound's low-pass filter, the part that
//!   actually reads as "muffled": water and dense matter swallow highs long
//!   before they swallow lows;
//! - **speed** — the sampler's playback rate, a pitch bend on top of the
//!   sound's own [`BasePitch`].
//!
//! All three taper from full strength at the centre of the field to nothing
//! at its edge, so a source drifting out of the field fades back to normal
//! instead of popping.
//!
//! ## Whose position counts
//!
//! A field is a *medium*, not a property of the things inside it, and sound
//! reaching an ear either crosses that medium or does not. Two positions
//! decide how much: the source's and the listener's. [`DampingTargets`] picks
//! which of them the field reads, and the two are combined with a
//! **maximum**, never a product:
//!
//! | source | listener | damping |
//! |--------|----------|---------|
//! | inside | outside  | full — the sound has to get out |
//! | outside| inside   | full — the sound has to get in |
//! | inside | inside   | full, **once** — you are both in the soup |
//! | outside| outside  | none |
//!
//! Taking the maximum is the whole trick: submerging the listener alongside
//! the source cannot damp anything twice, because one field contributes one
//! influence no matter how many of its endpoints are inside it.
//!
//! ## What is dampable
//!
//! Category-level exemptions go through
//! [`AudioCategory::is_dampable`](crate::AudioCategory::is_dampable())
//! (interface audio, typically); per-sound exemptions through the
//! [`UndampedSound`] marker. A sound whose volume node another system owns
//! outright carries [`SelfDrivenVolume`] and keeps its filter and pitch
//! damping while its volume is left alone. The low-pass axis reaches only
//! sounds whose pool effect chain carries a `LowPassNode` — `bevy_seedling`
//! fixes that chain when the pool is created, so hosts route dampable spatial
//! sounds through a pool of their own that includes one (with the filter
//! parked at [`OPEN_CUTOFF_HZ`]); the rest are volume- and pitch-damped only.
//!
//! A positionless sound is not *placed* anywhere, so no field can contain it;
//! it is damped only when the listener is inside one. That is the right
//! behaviour rather than a limitation: music ducking as the listener sinks
//! into a field is exactly the effect, and it would be meaningless for the
//! music to muffle because something else fell in.
//!
//! ## Geometry
//!
//! Field membership is measured in the XY plane: centres, source positions
//! and listener positions are all `GlobalTransform` translations with `z`
//! dropped, and only `SpatialListener2D` counts as a listener. A 3D game gets
//! nothing from the listener half of a field — a `SpatialListener3D` is not
//! seen, so [`DampingTargets::Listeners`] and the listener term of
//! [`DampingTargets::Both`] never fire — while the source half still works on
//! the XY projection of its positions. Measuring in three dimensions instead
//! is not a drop-in swap: a 2D game layers its sprites along `z`, and letting
//! that distance attenuate would muffle sounds by their draw order.
//!
//! What stays with the host: pool construction (the effect chain and its
//! distance model are authored per game), and any announcement layer (entry/
//! exit cues, immersion beds) built on top of [`SoundDampingField::influence`].
//!
//! ## Example
//!
//! ```
//! # use bevy::prelude::*;
//! # use msg_seedling::prelude::*;
//! # #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
//! # #[reflect(Component)]
//! # enum Sound { #[default] Sfx, Ui }
//! # #[derive(Resource, Clone, Default)]
//! # struct GameAudioConfig;
//! # impl AudioConfig for GameAudioConfig {
//! #     fn master_volume(&self) -> f32 { 1.0 }
//! # }
//! # impl AudioCategory for Sound {
//! #     type Config = GameAudioConfig;
//! #     fn volume(&self, _config: &GameAudioConfig) -> f32 { 1.0 }
//! #     fn is_dampable(&self) -> bool { !matches!(self, Sound::Ui) }
//! # }
//! # let mut app = App::new();
//! # app.add_plugins(MinimalPlugins);
//! app.add_plugins(DampingPlugin::<Sound>::default());
//!
//! fn flood_the_basement(mut commands: Commands) {
//!     // Everything within 12 units of the pool sounds like it is under it:
//!     // most of the level gone, the highs gone first, the pitch dragged
//!     // down a little.
//!     commands.spawn((
//!         Transform::from_xyz(40.0, -8.0, 0.0),
//!         SoundDampingField {
//!             radius: 12.0,
//!             volume: 0.35,
//!             cutoff_hz: 700.0,
//!             speed: 0.9,
//!             targets: DampingTargets::Both,
//!         },
//!     ));
//! }
//! # app.add_systems(Update, flood_the_basement);
//! # app.update();
//! ```

use bevy::prelude::*;
use bevy_seedling::prelude::*;

use crate::baseline::{BasePitch, BaseVolume};
use crate::ducking::{DuckingEnvelope, Ducks};
use crate::fade::{FadeInAudio, FadeOutAudio};
use crate::traits::AudioCategory;

/// Low-pass cutoff at which the filter is effectively transparent.
///
/// Above the top of human hearing, so an undamped sound keeps its full
/// spectrum while still passing through the same filter node.
pub const OPEN_CUTOFF_HZ: f32 = 20_000.0;

/// Bounds a field's authored playback speed is clamped into when applied,
/// mirroring [`geometric_lerp`]'s guard on the cutoff: a non-positive speed
/// has no geometric path from `1.0`.
const MIN_FIELD_SPEED: f32 = 0.01;
const MAX_FIELD_SPEED: f32 = 100.0;

/// Which end of the sound's path a field reads.
///
/// A field always damps a sound *once*, whichever endpoints it covers; this
/// only decides which endpoints it is allowed to look at.
#[derive(Reflect, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DampingTargets {
    /// Only sources inside the field are damped. The field is something that
    /// swallows the sound made in it, heard from anywhere.
    Sources,
    /// Only a listener inside the field is damped, for every sound it hears.
    /// The field is something you put your ears into.
    Listeners,
    /// Both ends count, combined with a maximum — the default, and the only
    /// one of the three that models a medium rather than a one-way effect.
    #[default]
    Both,
}

impl DampingTargets {
    /// Whether a source's own position places it in the field.
    #[must_use]
    pub fn reads_sources(self) -> bool {
        matches!(self, Self::Sources | Self::Both)
    }

    /// Whether the listener's position places every sound it hears in the field.
    #[must_use]
    pub fn reads_listeners(self) -> bool {
        matches!(self, Self::Listeners | Self::Both)
    }
}

/// A world-space volume that muffles the sound crossing it.
///
/// Place it on any entity with a transform; the entity's global translation
/// is the centre. Fields do not need a collider — membership is a distance
/// test rather than an overlap test.
///
/// The three sound knobs describe a sound *at the centre*; at `radius` and
/// beyond nothing is changed. `targets` decides whose position is measured
/// against that radius.
#[derive(Component, Reflect, Debug, Clone, Copy, PartialEq)]
#[reflect(Component)]
pub struct SoundDampingField {
    /// Radius of influence in world units. Non-positive disables the field.
    pub radius: f32,
    /// Linear volume multiplier at the centre. `0.0` silences, `1.0` leaves
    /// the volume alone.
    pub volume: f32,
    /// Low-pass cutoff in hertz at the centre. [`OPEN_CUTOFF_HZ`] leaves the
    /// spectrum alone; a few hundred hertz is the classic underwater muffle.
    pub cutoff_hz: f32,
    /// Playback-speed multiplier at the centre. Below `1.0` drags the pitch
    /// down, above `1.0` pushes it up, `1.0` leaves it alone. Clamped into
    /// `0.01..=100.0` when applied, guarding non-positive authored values.
    pub speed: f32,
    /// Whose position the field measures.
    pub targets: DampingTargets,
}

impl Default for SoundDampingField {
    fn default() -> Self {
        Self {
            radius: 0.0,
            volume: 1.0,
            cutoff_hz: OPEN_CUTOFF_HZ,
            speed: 1.0,
            targets: DampingTargets::Both,
        }
    }
}

impl SoundDampingField {
    /// How deep inside the field a point `distance` from the centre is: `1.0`
    /// at the centre, `0.0` at `radius` and beyond, smoothstepped in between.
    #[must_use]
    pub fn influence(&self, distance: f32) -> f32 {
        if self.radius <= 0.0 {
            return 0.0;
        }
        let t = (distance / self.radius).clamp(0.0, 1.0);
        // Smoothstep, flipped: flat at the centre, flat at the rim, so a
        // source crossing the boundary neither pops nor lurches.
        1.0 - t * t * (3.0 - 2.0 * t)
    }
}

/// Marks a sound no [`SoundDampingField`] may touch.
///
/// A field's own announcement voice is the case this exists for: a bed
/// announcing the muffling must not be muffled by the muffling it announces,
/// and a crossing cue has to cut through at the moment the rest of the mix
/// drops away. Fixed at spawn — a sound is either part of a field's voice or
/// subject to fields, never both and never one and then the other.
#[derive(Component, Reflect, Debug, Default, Clone, Copy)]
#[reflect(Component)]
pub struct UndampedSound;

/// Marks a sound whose volume node is written by its own system every frame.
///
/// A gain ramp recomputing itself from live state owns that node outright;
/// damping must not write it too — the two would overwrite each other on
/// alternate frames and flicker. Such a sound is still filtered and pitched
/// normally; only the volume is left to its owner.
#[derive(Component, Reflect, Debug, Default, Clone, Copy)]
#[reflect(Component)]
pub struct SelfDrivenVolume;

/// A field paired with how deeply the listener sits in it, resolved once per
/// frame because it is the same for every sound.
///
/// [`ActiveField::influence_on`] then folds in the source's own depth.
#[derive(Debug, Clone, Copy)]
pub struct ActiveField<'a> {
    /// World-space centre of the field.
    pub centre: Vec2,
    /// The field itself.
    pub field: &'a SoundDampingField,
    /// How deeply the listener sits in this field, already zeroed when the
    /// field does not read listeners.
    pub listener_influence: f32,
}

impl<'a> ActiveField<'a> {
    /// Pairs a field with a listener, zeroing the listener term for a field
    /// that does not read listeners.
    #[must_use]
    pub fn new(centre: Vec2, field: &'a SoundDampingField, listener: Option<Vec2>) -> Self {
        let listener_influence = match listener {
            Some(listener) if field.targets.reads_listeners() => {
                field.influence(centre.distance(listener))
            }
            _ => 0.0,
        };
        Self {
            centre,
            field,
            listener_influence,
        }
    }

    /// How strongly this field acts on a source at `position`.
    ///
    /// The maximum of the two endpoints' depths, so a field that has both the
    /// source and the listener inside it still damps exactly once.
    #[must_use]
    pub fn influence_on(&self, position: Vec2) -> f32 {
        let source_influence = if self.field.targets.reads_sources() {
            self.field.influence(self.centre.distance(position))
        } else {
            0.0
        };
        source_influence.max(self.listener_influence)
    }
}

/// The combined effect of every field acting on one point in the world.
///
/// [`SoundDamping::NONE`] is the identity: full volume, open filter, unbent
/// pitch.
#[derive(Reflect, Debug, Clone, Copy, PartialEq)]
pub struct SoundDamping {
    /// Linear volume multiplier.
    pub volume: f32,
    /// Low-pass cutoff in hertz.
    pub cutoff_hz: f32,
    /// Playback-speed multiplier.
    pub speed: f32,
}

impl Default for SoundDamping {
    fn default() -> Self {
        Self::NONE
    }
}

impl SoundDamping {
    /// Leaves a sound exactly as it was.
    pub const NONE: Self = Self {
        volume: 1.0,
        cutoff_hz: OPEN_CUTOFF_HZ,
        speed: 1.0,
    };

    /// Resolves every field acting on a source at `position` into a single
    /// damping.
    ///
    /// Overlapping fields do not stack: on each axis the strongest one wins —
    /// the quietest volume, the lowest cutoff, and the speed furthest from
    /// unbent. Stacking would let two mild fields silence a sound outright,
    /// which is not what either of them authored. Within one field the source
    /// and listener terms are likewise combined with a maximum, by
    /// [`ActiveField::influence_on`].
    #[must_use]
    pub fn resolve<'a>(
        fields: impl IntoIterator<Item = &'a ActiveField<'a>>,
        position: Vec2,
    ) -> Self {
        let mut damping = Self::NONE;
        for active in fields {
            damping.accumulate(active.field, active.influence_on(position));
        }
        damping
    }

    /// Resolves every field for a sound that has no position of its own.
    ///
    /// Music has no place in the world: it is not emitted from anywhere, so
    /// only the listener's own membership can damp it. Combining is otherwise
    /// identical to [`SoundDamping::resolve`].
    #[must_use]
    pub fn resolve_positionless<'a>(fields: impl IntoIterator<Item = &'a ActiveField<'a>>) -> Self {
        let mut damping = Self::NONE;
        for active in fields {
            damping.accumulate(active.field, active.listener_influence);
        }
        damping
    }

    /// Folds one field's contribution in, strongest-wins on each axis.
    fn accumulate(&mut self, field: &SoundDampingField, influence: f32) {
        if influence <= 0.0 {
            return;
        }

        self.volume = self
            .volume
            .min(1.0_f32.lerp(field.volume.clamp(0.0, 1.0), influence));
        // Cutoff interpolates geometrically: pitch and filter sweeps are
        // heard in octaves, so halving twice must feel like one even slide.
        self.cutoff_hz =
            self.cutoff_hz
                .min(geometric_lerp(OPEN_CUTOFF_HZ, field.cutoff_hz, influence));

        // Speed is heard in octaves just like the cutoff, so it interpolates
        // geometrically too: `1.0 * (speed / 1.0).powf(influence)`.
        let speed = field
            .speed
            .clamp(MIN_FIELD_SPEED, MAX_FIELD_SPEED)
            .powf(influence);
        if (speed - 1.0).abs() > (self.speed - 1.0).abs() {
            self.speed = speed;
        }
    }

    /// Whether this damping leaves a sound untouched.
    #[must_use]
    pub fn is_none(&self) -> bool {
        *self == Self::NONE
    }
}

/// Interpolates between two frequencies in log space, guarding the degenerate
/// bounds a hand-authored cutoff can produce.
fn geometric_lerp(from: f32, to: f32, t: f32) -> f32 {
    let to = to.clamp(1.0, OPEN_CUTOFF_HZ);
    from * (to / from).powf(t)
}

/// The listener whose ears a field centred at `centre` covers.
///
/// `bevy_seedling` spatializes each emitter against its nearest listener, so
/// a field measures itself against the same one. Positions are in the XY
/// plane (see the [module docs](self#geometry)); an empty slice — an app with
/// no `SpatialListener2D` — has no listener to be inside anything, so every
/// field falls back to its source term alone.
#[must_use]
pub fn nearest_listener(listeners: &[Vec2], centre: Vec2) -> Option<Vec2> {
    listeners.iter().copied().min_by(|a, b| {
        centre
            .distance_squared(*a)
            .total_cmp(&centre.distance_squared(*b))
    })
}

/// Applies every [`SoundDampingField`] — and the current [`DuckingEnvelope`]
/// — to the sounds playing inside them.
///
/// With no fields alive and the duck idle the system does nothing, except
/// for the one frame after the last of them lets go — that pass writes every
/// held sound back to its undamped level.
///
/// ## Who owns the volume node
///
/// Nobody, exclusively: the node is a product of independently-owned layers
/// (see [`baseline`](crate::baseline)), and every system that writes it
/// recomputes the whole product. This one writes
///
/// ```text
/// category volume × BaseVolume × damping × duck
/// ```
///
/// every frame it is active, and the same expression with `damping` and
/// `duck` at unity on the frame it lets go — which is exactly what the
/// config-driven write in [`VolumeSystems`](crate::VolumeSystems) produces,
/// so the hand-off is seamless in both directions. Because it is scheduled
/// after that set and reads the live config, a settings change made on a
/// damped frame is folded into the same frame's recompute instead of
/// fighting it.
///
/// Sounds mid [`FadeInAudio`]/[`FadeOutAudio`] leave only their volume to
/// the fade — the filter and pitch axes stay driven, so a fade overlapping
/// the release pass cannot strand a muffled cutoff or a bent pitch. The duck
/// rides the same volume write because two systems writing the same volume
/// nodes on alternate frames would flicker.
pub fn apply_sound_damping<C: AudioCategory>(
    config: Res<C::Config>,
    duck: Res<DuckingEnvelope>,
    q_fields: Query<(&GlobalTransform, &SoundDampingField)>,
    q_listeners: Query<&GlobalTransform, With<SpatialListener2D>>,
    mut q_sounds: Query<
        (
            Option<&GlobalTransform>,
            &C,
            &SampleEffects,
            Option<&BaseVolume>,
            Option<&BasePitch>,
            Option<&mut PlaybackSettings>,
            Has<SelfDrivenVolume>,
            Has<Ducks>,
            Has<FadeInAudio>,
            Has<FadeOutAudio>,
        ),
        (With<SamplePlayer>, Without<UndampedSound>),
    >,
    mut volume_nodes: Query<&mut VolumeNode>,
    mut low_pass_nodes: Query<&mut LowPassNode>,
    mut was_active: Local<bool>,
) {
    let has_fields = q_fields.iter().any(|(_, field)| field.radius > 0.0);
    let active = has_fields || !duck.is_idle();
    if !active && !*was_active {
        return;
    }
    *was_active = active;

    let listener_positions: Vec<Vec2> = q_listeners
        .iter()
        .map(|transform| transform.translation().truncate())
        .collect();

    let fields: Vec<ActiveField> = q_fields
        .iter()
        .filter(|(_, field)| field.radius > 0.0)
        .map(|(transform, field)| {
            let centre = transform.translation().truncate();
            ActiveField::new(centre, field, nearest_listener(&listener_positions, centre))
        })
        .collect();

    for (
        transform,
        category,
        effects,
        base_volume,
        base_pitch,
        settings,
        self_driven_volume,
        ducks,
        fading_in,
        fading_out,
    ) in &mut q_sounds
    {
        if !category.is_dampable() {
            continue;
        }

        // A sound with a transform is an event somewhere in the world and is
        // measured from there. One without — music, and anything else played
        // flat — has no place to be measured from, so only the listener's own
        // membership reaches it.
        let damping = match transform {
            Some(transform) => SoundDamping::resolve(&fields, transform.translation().truncate()),
            None => SoundDamping::resolve_positionless(&fields),
        };

        // A fade owns the volume node outright for its duration, and a
        // `SelfDrivenVolume` sound's own system owns it forever; neither is
        // written here. Both still get the filter and pitch axes below.
        let fading = fading_in || fading_out;
        if !self_driven_volume
            && !fading
            && let Ok(mut volume_node) = volume_nodes.get_effect_mut(effects)
        {
            let base = base_volume.copied().unwrap_or_default();
            let volume = Volume::Linear(
                base.resolve(category.volume(&config)) * damping.volume * duck.gain_for(ducks),
            );
            if volume_node.volume != volume {
                volume_node.volume = volume;
            }
        }

        // Only sounds whose pool chain carries a filter can be muffled; the
        // rest are volume- and pitch-damped only.
        if let Ok(mut low_pass) = low_pass_nodes.get_effect_mut(effects)
            && low_pass.frequency != damping.cutoff_hz
        {
            low_pass.frequency = damping.cutoff_hz;
        }

        if let (Some(base_pitch), Some(mut settings)) = (base_pitch, settings) {
            // `PlaybackSettings::speed` is `f32` in some firewheel releases
            // admitted by the bevy_seedling 0.7 range and `f64` in others;
            // an f32 product widens into either losslessly via `Into`.
            let speed = (base_pitch.0 * damping.speed).into();
            if settings.speed != speed {
                settings.speed = speed;
            }
        }
    }
}

/// Adds damping (and its ducking dependency) for category `C`.
///
/// Registers the field/marker types and schedules
/// [`apply_sound_damping<C>`] after [`VolumeSystems`](crate::VolumeSystems)
/// and after the ducking tick, so the one volume write per frame folds in
/// the freshest envelope and wins over the bare config-driven write.
pub struct DampingPlugin<C: AudioCategory> {
    _phantom: std::marker::PhantomData<C>,
}

impl<C: AudioCategory> Default for DampingPlugin<C> {
    fn default() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<C: AudioCategory> Plugin for DampingPlugin<C> {
    fn build(&self, app: &mut App) {
        crate::ducking::plugin(app);

        app.register_type::<SoundDampingField>();
        app.register_type::<DampingTargets>();
        app.register_type::<SoundDamping>();
        app.register_type::<UndampedSound>();
        app.register_type::<SelfDrivenVolume>();
        crate::baseline::register_types(app);
        app.init_resource::<C::Config>();
        app.add_systems(
            Update,
            apply_sound_damping::<C>
                .after(crate::VolumeSystems)
                .after(crate::ducking::tick_ducking_envelope),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::AudioConfig;
    use msg_testing::{AppTesting, physics_app};

    const FAR: Vec2 = Vec2::new(1000.0, 0.0);

    fn field(radius: f32, volume: f32, cutoff_hz: f32, speed: f32) -> SoundDampingField {
        SoundDampingField {
            radius,
            volume,
            cutoff_hz,
            speed,
            ..Default::default()
        }
    }

    /// Resolves one field with the listener nowhere near it.
    fn damping_at(damper: &SoundDampingField, source: Vec2) -> SoundDamping {
        let active = ActiveField::new(Vec2::ZERO, damper, Some(FAR));
        SoundDamping::resolve([&active], source)
    }

    #[test]
    fn influence_is_full_at_the_centre_and_zero_at_the_rim() {
        let field = field(10.0, 0.0, 500.0, 0.8);
        assert_eq!(field.influence(0.0), 1.0);
        assert_eq!(field.influence(10.0), 0.0);
        assert_eq!(field.influence(50.0), 0.0);
        let halfway = field.influence(5.0);
        assert!(
            (halfway - 0.5).abs() < 1e-6,
            "smoothstep is symmetric about the midpoint, got {halfway}"
        );
    }

    #[test]
    fn a_zero_radius_field_never_acts() {
        assert_eq!(field(0.0, 0.0, 100.0, 0.5).influence(0.0), 0.0);
        assert_eq!(field(-5.0, 0.0, 100.0, 0.5).influence(0.0), 0.0);
    }

    #[test]
    fn a_source_outside_every_field_is_untouched() {
        let damper = field(10.0, 0.0, 200.0, 0.5);
        assert!(damping_at(&damper, Vec2::new(20.0, 0.0)).is_none());
    }

    #[test]
    fn a_source_at_the_centre_gets_the_authored_values() {
        let damper = field(10.0, 0.25, 400.0, 0.75);
        let damping = damping_at(&damper, Vec2::ZERO);
        assert!((damping.volume - 0.25).abs() < 1e-6);
        assert!((damping.cutoff_hz - 400.0).abs() < 1e-3);
        assert!((damping.speed - 0.75).abs() < 1e-6);
    }

    #[test]
    fn damping_tapers_between_centre_and_rim() {
        let damper = field(10.0, 0.0, 200.0, 0.5);
        let damping = damping_at(&damper, Vec2::new(5.0, 0.0));
        assert!(
            damping.volume > 0.0 && damping.volume < 1.0,
            "half-way volume should sit between silence and full, got {}",
            damping.volume
        );
        assert!(
            damping.cutoff_hz > 200.0 && damping.cutoff_hz < OPEN_CUTOFF_HZ,
            "half-way cutoff should sit between the authored and open cutoff, got {}",
            damping.cutoff_hz
        );
        assert!(
            damping.speed > 0.5 && damping.speed < 1.0,
            "half-way speed should sit between the authored and unbent speed, got {}",
            damping.speed
        );
    }

    #[test]
    fn overlapping_fields_do_not_stack() {
        let mild = field(10.0, 0.5, 1000.0, 0.9);
        let here = ActiveField::new(Vec2::ZERO, &mild, Some(FAR));
        let nearby = ActiveField::new(Vec2::new(1.0, 0.0), &mild, Some(FAR));
        let damping = SoundDamping::resolve([&here, &nearby], Vec2::ZERO);
        assert!(
            damping.volume >= 0.5,
            "two 0.5 fields must not multiply down to 0.25, got {}",
            damping.volume
        );
    }

    #[test]
    fn the_strongest_field_wins_each_axis() {
        let mild = field(10.0, 0.8, 8000.0, 0.95);
        let strong = field(10.0, 0.1, 300.0, 0.6);
        let mild = ActiveField::new(Vec2::ZERO, &mild, Some(FAR));
        let strong = ActiveField::new(Vec2::ZERO, &strong, Some(FAR));
        let damping = SoundDamping::resolve([&mild, &strong], Vec2::ZERO);
        assert!((damping.volume - 0.1).abs() < 1e-6);
        assert!((damping.cutoff_hz - 300.0).abs() < 1e-3);
        assert!((damping.speed - 0.6).abs() < 1e-6);
    }

    #[test]
    fn an_absurd_cutoff_stays_inside_the_audible_range() {
        let broken = field(10.0, 1.0, -100.0, 1.0);
        let damping = damping_at(&broken, Vec2::ZERO);
        assert!(
            damping.cutoff_hz >= 1.0 && damping.cutoff_hz <= OPEN_CUTOFF_HZ,
            "cutoff {} escaped the audible range",
            damping.cutoff_hz
        );
    }

    // ==================== Source / listener membership ====================

    #[test]
    fn a_listener_inside_a_field_hears_the_world_outside_damped() {
        let damper = field(10.0, 0.2, 400.0, 0.8);
        let active = ActiveField::new(Vec2::ZERO, &damper, Some(Vec2::ZERO));
        let damping = SoundDamping::resolve([&active], FAR);
        assert!((damping.volume - 0.2).abs() < 1e-6);
        assert!((damping.cutoff_hz - 400.0).abs() < 1e-3);
    }

    #[test]
    fn both_endpoints_inside_damp_exactly_once() {
        let damper = field(10.0, 0.2, 400.0, 0.8);
        let source_only = ActiveField::new(Vec2::ZERO, &damper, Some(FAR));
        let both = ActiveField::new(Vec2::ZERO, &damper, Some(Vec2::ZERO));

        let outside_ear = SoundDamping::resolve([&source_only], Vec2::ZERO);
        let inside_ear = SoundDamping::resolve([&both], Vec2::ZERO);

        assert_eq!(
            inside_ear, outside_ear,
            "submerging the listener next to the source must not damp twice"
        );
    }

    #[test]
    fn source_only_fields_ignore_the_listener() {
        let damper = SoundDampingField {
            targets: DampingTargets::Sources,
            ..field(10.0, 0.2, 400.0, 0.8)
        };
        let active = ActiveField::new(Vec2::ZERO, &damper, Some(Vec2::ZERO));
        assert!(
            SoundDamping::resolve([&active], FAR).is_none(),
            "a Sources field must not damp a distant source just because the listener is inside"
        );
        assert!(!SoundDamping::resolve([&active], Vec2::ZERO).is_none());
    }

    #[test]
    fn listener_only_fields_ignore_the_source() {
        let damper = SoundDampingField {
            targets: DampingTargets::Listeners,
            ..field(10.0, 0.2, 400.0, 0.8)
        };
        let outside_ear = ActiveField::new(Vec2::ZERO, &damper, Some(FAR));
        assert!(
            SoundDamping::resolve([&outside_ear], Vec2::ZERO).is_none(),
            "a Listeners field must not damp a source inside it while the ear is outside"
        );
        let inside_ear = ActiveField::new(Vec2::ZERO, &damper, Some(Vec2::ZERO));
        assert!(!SoundDamping::resolve([&inside_ear], FAR).is_none());
    }

    #[test]
    fn a_positionless_sound_follows_the_listener_only() {
        let damper = field(10.0, 0.2, 400.0, 0.8);

        // Something falling into the field cannot muffle the music...
        let outside_ear = ActiveField::new(Vec2::ZERO, &damper, Some(FAR));
        assert!(SoundDamping::resolve_positionless([&outside_ear]).is_none());

        // ...but sinking into it yourself does.
        let inside_ear = ActiveField::new(Vec2::ZERO, &damper, Some(Vec2::ZERO));
        let damping = SoundDamping::resolve_positionless([&inside_ear]);
        assert!((damping.volume - 0.2).abs() < 1e-6);
    }

    #[test]
    fn a_field_with_no_listener_still_damps_its_sources() {
        let damper = field(10.0, 0.2, 400.0, 0.8);
        let active = ActiveField::new(Vec2::ZERO, &damper, None);
        assert!((SoundDamping::resolve([&active], Vec2::ZERO).volume - 0.2).abs() < 1e-6);
    }

    #[test]
    fn nearest_listener_picks_the_closest() {
        let listeners = [Vec2::new(5.0, 0.0), Vec2::new(1.0, 0.0)];
        assert_eq!(
            nearest_listener(&listeners, Vec2::ZERO),
            Some(Vec2::new(1.0, 0.0))
        );
        assert_eq!(nearest_listener(&[], Vec2::ZERO), None);
    }

    // ==================== The apply system ====================

    #[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
    #[reflect(Component)]
    enum TestSound {
        #[default]
        World,
        Interface,
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
        fn is_dampable(&self) -> bool {
            matches!(self, TestSound::World)
        }
    }

    fn damping_app() -> App {
        let mut app = physics_app();
        app.add_plugins(DampingPlugin::<TestSound>::default());
        app
    }

    fn spawn_field(app: &mut App, centre: Vec2, damper: SoundDampingField) {
        app.world_mut().spawn((
            damper,
            Transform::from_translation(centre.extend(0.0)),
            GlobalTransform::from_translation(centre.extend(0.0)),
        ));
    }

    fn spawn_sound(app: &mut App, category: TestSound, position: Vec2) -> Entity {
        spawn_sound_with_volume(app, category, position, 1.0)
    }

    /// Spawns a sound the way [`handle_play_audio`](crate::audio_systems::handle_play_audio)
    /// does: a node seeded at `category × base`, and the `base` recorded on
    /// the entity. The test config puts every category at `1.0`, so the two
    /// numbers coincide here.
    fn spawn_sound_with_volume(
        app: &mut App,
        category: TestSound,
        position: Vec2,
        volume: f32,
    ) -> Entity {
        app.world_mut()
            .spawn((
                SamplePlayer::new(Handle::default()),
                category,
                BaseVolume(volume),
                Transform::from_translation(position.extend(0.0)),
                GlobalTransform::from_translation(position.extend(0.0)),
                sample_effects![VolumeNode::from_linear(volume)],
            ))
            .id()
    }

    fn volume_of(app: &mut App, entity: Entity) -> f32 {
        let world = app.world_mut();
        let effect = {
            let effects = world.get::<SampleEffects>(entity).unwrap();
            effects[0]
        };
        let mut nodes = world.query::<&VolumeNode>();
        let node = nodes
            .get(world, effect)
            .expect("effect entity has a VolumeNode");
        node.volume.linear()
    }

    fn cutoff_of(app: &mut App, entity: Entity) -> f32 {
        let world = app.world_mut();
        let effects = world
            .get::<SampleEffects>(entity)
            .expect("sound has effects")
            .iter()
            .collect::<Vec<_>>();
        let mut nodes = world.query::<&LowPassNode>();
        effects
            .into_iter()
            .find_map(|effect| nodes.get(world, effect).ok())
            .expect("effect chain has a LowPassNode")
            .frequency
    }

    #[test]
    fn a_sound_in_a_field_is_attenuated_and_restored() {
        let mut app = damping_app();
        spawn_field(&mut app, Vec2::ZERO, field(10.0, 0.25, 400.0, 1.0));
        let sound = spawn_sound(&mut app, TestSound::World, Vec2::ZERO);
        app.update_n(2);
        assert!((volume_of(&mut app, sound) - 0.25).abs() < 1e-6);

        // The field collapses: the restoring pass brings the sound back.
        let world = app.world_mut();
        let mut fields = world.query::<&mut SoundDampingField>();
        fields.single_mut(world).expect("field exists").radius = 0.0;
        app.update_n(2);
        assert!((volume_of(&mut app, sound) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn exempt_sounds_are_left_alone() {
        let mut app = damping_app();
        spawn_field(&mut app, Vec2::ZERO, field(10.0, 0.25, 400.0, 1.0));

        // A non-dampable category is skipped entirely.
        let interface = spawn_sound(&mut app, TestSound::Interface, Vec2::ZERO);
        // An UndampedSound is skipped whatever its category says.
        let undamped = spawn_sound(&mut app, TestSound::World, Vec2::ZERO);
        app.world_mut().entity_mut(undamped).insert(UndampedSound);
        // A SelfDrivenVolume sound keeps its volume for its owner.
        let self_driven = spawn_sound(&mut app, TestSound::World, Vec2::ZERO);
        app.world_mut()
            .entity_mut(self_driven)
            .insert(SelfDrivenVolume);

        app.update_n(2);
        assert!((volume_of(&mut app, interface) - 1.0).abs() < 1e-6);
        assert!((volume_of(&mut app, undamped) - 1.0).abs() < 1e-6);
        assert!((volume_of(&mut app, self_driven) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_duck_rides_the_same_volume_write() {
        let mut app = damping_app();
        let ducked = spawn_sound(&mut app, TestSound::World, Vec2::ZERO);
        app.world_mut().entity_mut(ducked).insert(Ducks);
        let clear = spawn_sound(&mut app, TestSound::World, Vec2::ZERO);

        app.world_mut().resource_mut::<DuckingEnvelope>().trigger();
        // Enough fixed steps (~15.6ms each) to bottom the 40ms attack out.
        app.update_n(5);

        let ducked_gain = app.world().resource::<DuckingEnvelope>().ducked_gain;
        assert!((volume_of(&mut app, ducked) - ducked_gain).abs() < 1e-6);
        assert!((volume_of(&mut app, clear) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn speed_interpolates_geometrically() {
        // Two octaves down at the centre: half influence is one octave, not
        // the linear midpoint 0.625.
        let damper = field(10.0, 1.0, OPEN_CUTOFF_HZ, 0.25);
        let damping = damping_at(&damper, Vec2::new(5.0, 0.0));
        assert!(
            (damping.speed - 0.5).abs() < 1e-6,
            "half influence over two octaves should be one octave, got {}",
            damping.speed
        );

        // A non-positive authored speed is clamped, never zero or negative.
        let broken = field(10.0, 1.0, OPEN_CUTOFF_HZ, -3.0);
        assert!(damping_at(&broken, Vec2::ZERO).speed > 0.0);
    }

    #[test]
    fn a_sound_outside_every_field_keeps_its_spawn_volume() {
        let mut app = damping_app();
        // A field alive elsewhere makes the system touch every dampable
        // sound; ones it does not reach must keep their exact spawn volume.
        spawn_field(
            &mut app,
            Vec2::new(500.0, 0.0),
            field(10.0, 0.0, 400.0, 1.0),
        );
        let sound = spawn_sound_with_volume(&mut app, TestSound::World, Vec2::ZERO, 0.3);
        app.update_n(2);
        assert!((volume_of(&mut app, sound) - 0.3).abs() < 1e-6);

        // Same for a duck the sound stands clear of (no `Ducks` marker).
        app.world_mut().resource_mut::<DuckingEnvelope>().trigger();
        app.update_n(5);
        assert!((volume_of(&mut app, sound) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn a_sound_without_a_baseline_reads_as_full() {
        let mut app = damping_app();
        spawn_field(&mut app, Vec2::ZERO, field(10.0, 0.25, 400.0, 1.0));
        // A hand-spawned sound with no `BaseVolume` is documented to read as
        // `1.0`, so the field scales the bare category volume.
        let sound = app
            .world_mut()
            .spawn((
                SamplePlayer::new(Handle::default()),
                TestSound::World,
                Transform::default(),
                GlobalTransform::default(),
                sample_effects![VolumeNode::from_linear(1.0)],
            ))
            .id();
        app.update_n(2);
        assert!((volume_of(&mut app, sound) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_per_sound_volume_survives_a_damp_and_release_cycle() {
        let mut app = damping_app();
        spawn_field(&mut app, Vec2::ZERO, field(10.0, 0.25, 400.0, 1.0));
        let sound = spawn_sound_with_volume(&mut app, TestSound::World, Vec2::ZERO, 0.3);
        app.update_n(2);
        assert!(
            (volume_of(&mut app, sound) - 0.3 * 0.25).abs() < 1e-6,
            "the field scales the sound's own volume, not the bare category"
        );

        // The field collapses: the release pass recomputes `category × base`
        // with both damping factors back at unity.
        let world = app.world_mut();
        let mut fields = world.query::<&mut SoundDampingField>();
        fields.single_mut(world).expect("field exists").radius = 0.0;
        app.update_n(2);
        assert!((volume_of(&mut app, sound) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn the_duck_scales_the_spawn_volume_and_releases_it() {
        let mut app = damping_app();
        let sound = spawn_sound_with_volume(&mut app, TestSound::World, Vec2::ZERO, 0.3);
        app.world_mut().entity_mut(sound).insert(Ducks);

        app.world_mut().resource_mut::<DuckingEnvelope>().trigger();
        app.update_n(5);
        let ducked_gain = app.world().resource::<DuckingEnvelope>().ducked_gain;
        assert!((volume_of(&mut app, sound) - 0.3 * ducked_gain).abs() < 1e-6);

        // Ride the whole hold and release out; the restore pass returns the
        // exact spawn volume.
        app.update_n(60);
        assert!(app.world().resource::<DuckingEnvelope>().is_idle());
        assert!((volume_of(&mut app, sound) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn a_fade_owns_only_the_volume_axis() {
        use core::time::Duration;

        let mut app = damping_app();
        spawn_field(&mut app, Vec2::ZERO, field(10.0, 0.25, 400.0, 0.5));
        // No fade systems run in this app, so the marker stays put — the
        // sound is mid-fade for the whole test.
        let sound = app
            .world_mut()
            .spawn((
                SamplePlayer::new(Handle::default()),
                TestSound::World,
                Transform::default(),
                GlobalTransform::default(),
                sample_effects![
                    VolumeNode::from_linear(0.8),
                    LowPassNode {
                        frequency: OPEN_CUTOFF_HZ
                    }
                ],
                BasePitch(1.0),
                PlaybackSettings::default(),
                FadeOutAudio::new(Duration::from_secs(60)).keep_entity(),
            ))
            .id();
        app.update_n(2);

        // The fade keeps the volume node; filter and pitch still damp.
        assert!((volume_of(&mut app, sound) - 0.8).abs() < 1e-6);
        assert!((cutoff_of(&mut app, sound) - 400.0).abs() < 1e-3);
        let speed = app.world().get::<PlaybackSettings>(sound).unwrap().speed;
        assert!((speed - 0.5).abs() < 1e-6);

        // The field collapses mid-fade: the restore pass must still reopen
        // the filter and unbend the pitch.
        let world = app.world_mut();
        let mut fields = world.query::<&mut SoundDampingField>();
        fields.single_mut(world).expect("field exists").radius = 0.0;
        app.update_n(2);
        assert!((cutoff_of(&mut app, sound) - OPEN_CUTOFF_HZ).abs() < 1e-3);
        let speed = app.world().get::<PlaybackSettings>(sound).unwrap().speed;
        assert!((speed - 1.0).abs() < 1e-6);
        assert!((volume_of(&mut app, sound) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn base_pitch_is_bent_by_the_field() {
        let mut app = damping_app();
        spawn_field(&mut app, Vec2::ZERO, field(10.0, 1.0, OPEN_CUTOFF_HZ, 0.5));
        let sound = spawn_sound(&mut app, TestSound::World, Vec2::ZERO);
        app.world_mut()
            .entity_mut(sound)
            .insert((BasePitch(2.0), PlaybackSettings::default()));
        app.update_n(2);

        let speed = app.world().get::<PlaybackSettings>(sound).unwrap().speed;
        assert!(
            (speed - 1.0).abs() < 1e-6,
            "2.0 base pitch bent by the 0.5 field should land at 1.0, got {speed}"
        );

        // A sound without BasePitch is never pitch-bent.
        let unbent = spawn_sound(&mut app, TestSound::World, Vec2::ZERO);
        app.world_mut()
            .entity_mut(unbent)
            .insert(PlaybackSettings::default());
        app.update_n(2);
        let speed = app.world().get::<PlaybackSettings>(unbent).unwrap().speed;
        assert!((speed - 1.0).abs() < 1e-6);
    }
}
