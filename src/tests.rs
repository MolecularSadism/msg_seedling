use bevy::prelude::*;

use crate::messages::{FadeAudio, PlayAudio, SpatialPosition, StopAudio};
use crate::randomization::{DefaultRandomization, Randomization};
use crate::traits::{AudioCategory, AudioConfig};

// -- Test types --

#[derive(Component, Clone, Copy, Default, Debug, PartialEq, Eq, Hash, Reflect)]
#[reflect(Component)]
enum TestSound {
    #[default]
    Music,
    Sfx,
    Ambience,
}

#[derive(Resource, Clone, Default, Reflect)]
#[reflect(Resource)]
struct TestConfig {
    master: f32,
    music: f32,
    sfx: f32,
    ambience: f32,
    muted: bool,
}

impl AudioConfig for TestConfig {
    fn master_volume(&self) -> f32 {
        self.master
    }

    fn is_muted(&self) -> bool {
        self.muted
    }
}

impl AudioCategory for TestSound {
    type Config = TestConfig;

    fn volume(&self, config: &Self::Config) -> f32 {
        match self {
            TestSound::Music => config.music,
            TestSound::Sfx => config.sfx,
            TestSound::Ambience => config.ambience,
        }
    }
}

// -- AudioConfig trait tests --

#[test]
fn effective_volume_when_not_muted() {
    let config = TestConfig {
        master: 0.8,
        muted: false,
        ..Default::default()
    };
    assert!((config.effective_volume() - 0.8).abs() < f32::EPSILON);
}

#[test]
fn effective_volume_when_muted() {
    let config = TestConfig {
        master: 0.8,
        muted: true,
        ..Default::default()
    };
    assert!((config.effective_volume() - 0.0).abs() < f32::EPSILON);
}

#[test]
fn default_is_muted_returns_false() {
    #[derive(Resource, Clone, Default)]
    struct SimpleConfig;
    impl AudioConfig for SimpleConfig {
        fn master_volume(&self) -> f32 {
            0.5
        }
    }

    let config = SimpleConfig;
    assert!(!config.is_muted());
    assert!((config.effective_volume() - 0.5).abs() < f32::EPSILON);
}

#[test]
fn master_volume_independent_of_mute() {
    let config = TestConfig {
        master: 0.75,
        muted: true,
        ..Default::default()
    };
    assert!((config.master_volume() - 0.75).abs() < f32::EPSILON);
    assert!((config.effective_volume() - 0.0).abs() < f32::EPSILON);
}

// -- AudioCategory trait tests --

#[test]
fn category_volume_multiplier() {
    let config = TestConfig {
        master: 0.8,
        music: 0.5,
        sfx: 0.75,
        ambience: 0.3,
        muted: false,
    };

    assert!((TestSound::Music.volume(&config) - 0.5).abs() < f32::EPSILON);
    assert!((TestSound::Sfx.volume(&config) - 0.75).abs() < f32::EPSILON);
    assert!((TestSound::Ambience.volume(&config) - 0.3).abs() < f32::EPSILON);
}

// -- Randomization tests --

/// Deviation resolution only exists in the `rand` build; without the feature
/// no deviation is ever drawn, so a request's `Randomization` is carried and
/// ignored.
#[cfg(feature = "rand")]
mod randomization_resolution {
    use super::*;
    use crate::randomization::{deviate, resolve_randomization};
    use rand::SeedableRng;

    #[test]
    fn randomization_default_uses_plugin_defaults() {
        let defaults = DefaultRandomization {
            volume: Some(0.2),
            speed: Some(0.3),
        };
        let (vol, spd) = resolve_randomization(Randomization::Default, &defaults);
        assert_eq!(vol, Some(0.2));
        assert_eq!(spd, Some(0.3));
    }

    #[test]
    fn randomization_volume_overrides_volume_keeps_speed_default() {
        let defaults = DefaultRandomization {
            volume: Some(0.2),
            speed: Some(0.3),
        };
        let (vol, spd) = resolve_randomization(Randomization::Volume(0.5), &defaults);
        assert_eq!(vol, Some(0.5));
        assert_eq!(spd, Some(0.3));
    }

    #[test]
    fn randomization_speed_overrides_speed_keeps_volume_default() {
        let defaults = DefaultRandomization {
            volume: Some(0.2),
            speed: Some(0.3),
        };
        let (vol, spd) = resolve_randomization(Randomization::Speed(0.1), &defaults);
        assert_eq!(vol, Some(0.2));
        assert_eq!(spd, Some(0.1));
    }

    #[test]
    fn randomization_volume_and_speed_overrides_both() {
        let defaults = DefaultRandomization {
            volume: Some(0.2),
            speed: Some(0.3),
        };
        let (vol, spd) = resolve_randomization(
            Randomization::VolumeAndSpeed {
                volume: 0.4,
                speed: 0.6,
            },
            &defaults,
        );
        assert_eq!(vol, Some(0.4));
        assert_eq!(spd, Some(0.6));
    }

    #[test]
    fn randomization_with_none_defaults() {
        let defaults = DefaultRandomization {
            volume: None,
            speed: None,
        };
        let (vol, spd) = resolve_randomization(Randomization::Default, &defaults);
        assert_eq!(vol, None);
        assert_eq!(spd, None);
    }

    #[test]
    fn deviate_returns_the_centre_without_a_deviation() {
        let mut rng = rand::rngs::SmallRng::seed_from_u64(7);
        assert_eq!(deviate(&mut rng, 0.6, None), 0.6);
        assert_eq!(deviate(&mut rng, 0.6, Some(0.0)), 0.6);
        assert_eq!(deviate(&mut rng, 0.6, Some(f32::NAN)), 0.6);
    }

    #[test]
    fn deviate_stays_inside_its_band_and_never_goes_negative() {
        let mut rng = rand::rngs::SmallRng::seed_from_u64(11);
        for _ in 0..64 {
            let drawn = deviate(&mut rng, 0.5, Some(0.2));
            assert!(
                (0.4..=0.6).contains(&drawn),
                "a ±20% draw around 0.5 escaped its band: {drawn}"
            );
        }
        // A deviation wider than 1.0 floors at zero rather than flipping sign.
        for _ in 0..64 {
            let drawn = deviate(&mut rng, 0.5, Some(3.0));
            assert!((0.0..=2.0).contains(&drawn), "wide draw escaped: {drawn}");
        }
    }

    #[test]
    fn deviate_around_zero_has_nothing_to_draw() {
        let mut rng = rand::rngs::SmallRng::seed_from_u64(13);
        assert_eq!(deviate(&mut rng, 0.0, Some(0.5)), 0.0);
    }
}

#[test]
fn default_randomization_resource_defaults() {
    let defaults = DefaultRandomization::default();
    assert_eq!(defaults.volume, Some(0.2));
    assert_eq!(defaults.speed, Some(0.2));
}

// -- SpatialPosition tests --

#[test]
fn spatial_position_from_vec2() {
    let pos = SpatialPosition::from(Vec2::new(1.0, 2.0));
    let v3 = pos.as_vec3();
    assert!((v3.x - 1.0).abs() < f32::EPSILON);
    assert!((v3.y - 2.0).abs() < f32::EPSILON);
    assert!((v3.z - 0.0).abs() < f32::EPSILON);
}

#[test]
fn spatial_position_from_vec3() {
    let pos = SpatialPosition::from(Vec3::new(1.0, 2.0, 3.0));
    let v3 = pos.as_vec3();
    assert!((v3.x - 1.0).abs() < f32::EPSILON);
    assert!((v3.y - 2.0).abs() < f32::EPSILON);
    assert!((v3.z - 3.0).abs() < f32::EPSILON);
}

// -- PlayAudio builder tests --

#[test]
fn play_audio_defaults() {
    let msg = PlayAudio::new(Handle::default(), TestSound::Sfx);
    assert_eq!(msg.category, TestSound::Sfx);
    assert!(!msg.looping);
    assert!(msg.parent.is_none());
    assert!(msg.position.is_none());
    assert!((msg.volume - 1.0).abs() < f32::EPSILON);
    assert!(matches!(msg.randomization, Randomization::Default));
}

#[test]
fn play_audio_looping() {
    let msg = PlayAudio::new(Handle::default(), TestSound::Music).looping();
    assert!(msg.looping);
}

#[test]
fn play_audio_with_parent() {
    let entity = Entity::from_bits(42);
    let msg = PlayAudio::new(Handle::default(), TestSound::Sfx).with_parent(entity);
    assert_eq!(msg.parent, Some(entity));
}

#[test]
fn play_audio_at_vec2() {
    let msg = PlayAudio::new(Handle::default(), TestSound::Sfx).at(Vec2::new(10.0, 20.0));
    assert!(msg.position.is_some());
    let v3 = msg.position.unwrap().as_vec3();
    assert!((v3.x - 10.0).abs() < f32::EPSILON);
    assert!((v3.y - 20.0).abs() < f32::EPSILON);
}

#[test]
fn play_audio_at_vec3() {
    let msg = PlayAudio::new(Handle::default(), TestSound::Sfx).at(Vec3::new(1.0, 2.0, 3.0));
    assert!(msg.position.is_some());
    let v3 = msg.position.unwrap().as_vec3();
    assert!((v3.z - 3.0).abs() < f32::EPSILON);
}

#[test]
fn play_audio_with_volume() {
    let msg = PlayAudio::new(Handle::default(), TestSound::Sfx).with_volume(0.5);
    assert!((msg.volume - 0.5).abs() < f32::EPSILON);
}

#[test]
fn play_audio_randomized() {
    let msg =
        PlayAudio::new(Handle::default(), TestSound::Sfx).randomized(Randomization::Volume(0.3));
    assert!(
        matches!(msg.randomization, Randomization::Volume(v) if (v - 0.3).abs() < f32::EPSILON)
    );
}

#[test]
fn play_audio_builder_chaining() {
    let entity = Entity::from_bits(1);
    let msg = PlayAudio::new(Handle::default(), TestSound::Music)
        .looping()
        .with_volume(0.8)
        .with_parent(entity)
        .randomized(Randomization::Speed(0.1));

    assert!(msg.looping);
    assert!((msg.volume - 0.8).abs() < f32::EPSILON);
    assert_eq!(msg.parent, Some(entity));
    assert!(matches!(msg.randomization, Randomization::Speed(s) if (s - 0.1).abs() < f32::EPSILON));
}

// -- StopAudio tests --

#[test]
fn stop_audio_category() {
    let msg = StopAudio::category(TestSound::Music);
    assert_eq!(msg.category, Some(TestSound::Music));
}

#[test]
fn stop_audio_all() {
    let msg = StopAudio::<TestSound>::all();
    assert!(msg.category.is_none());
}

// -- FadeAudio tests --

#[test]
fn fade_audio_new() {
    let msg = FadeAudio::new(TestSound::Music, 2.5);
    assert_eq!(msg.category, TestSound::Music);
    assert!((msg.duration_secs - 2.5).abs() < f32::EPSILON);
}

// -- Plugin build test --

#[test]
fn plugin_inserts_default_randomization() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.init_resource::<TestConfig>();

    let plugin = crate::MsgSeedlingPlugin::<TestSound>::default();
    app.add_plugins(plugin);
    app.update();

    let defaults = app.world().resource::<DefaultRandomization>();
    assert_eq!(defaults.volume, Some(0.2));
    assert_eq!(defaults.speed, Some(0.2));
}

#[test]
fn plugin_custom_randomization() {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.init_resource::<TestConfig>();

    let plugin = crate::MsgSeedlingPlugin::<TestSound>::new().with_default_randomization(
        DefaultRandomization {
            volume: Some(0.1),
            speed: None,
        },
    );
    app.add_plugins(plugin);
    app.update();

    let defaults = app.world().resource::<DefaultRandomization>();
    assert_eq!(defaults.volume, Some(0.1));
    assert_eq!(defaults.speed, None);
}

// -- Spawn-time baselines --

mod spawned_baselines {
    use bevy_seedling::prelude::*;
    // Explicit: `bevy::prelude` exports a `PlaybackSettings` of its own and the
    // two globs would otherwise resolve to it.
    use bevy_seedling::prelude::PlaybackSettings;

    use super::*;
    use crate::baseline::{BasePitch, BaseVolume};

    fn play_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.insert_resource(TestConfig {
            sfx: 0.8,
            ..Default::default()
        });
        app.add_plugins(crate::MsgSeedlingPlugin::<TestSound>::default());
        app.update();
        app
    }

    /// The one sound the app spawned, with everything a request resolves.
    fn spawned(app: &mut App) -> (BaseVolume, BasePitch, f64, f32) {
        let world = app.world_mut();
        let (base, pitch, settings, effects) = world
            .query::<(&BaseVolume, &BasePitch, &PlaybackSettings, &SampleEffects)>()
            .single(world)
            .expect("exactly one spawned sound");
        let (base, pitch, speed) = (*base, *pitch, settings.speed);
        let effects: Vec<Entity> = effects.iter().collect();
        let node_volume = effects
            .iter()
            .find_map(|effect| world.get::<VolumeNode>(*effect))
            .expect("volume node effect")
            .volume
            .linear();
        (base, pitch, speed, node_volume)
    }

    fn exact() -> Randomization {
        Randomization::VolumeAndSpeed {
            volume: 0.0,
            speed: 0.0,
        }
    }

    #[test]
    fn a_request_records_its_own_volume_and_seeds_the_node_with_it() {
        let mut app = play_app();
        app.world_mut().write_message(
            PlayAudio::new(Handle::default(), TestSound::Sfx)
                .with_volume(0.5)
                .randomized(exact()),
        );
        app.update();

        let (base, pitch, speed, node_volume) = spawned(&mut app);
        assert_eq!(base, BaseVolume(0.5), "the request's own level is recorded");
        assert_eq!(pitch, BasePitch(1.0));
        assert!((speed - 1.0).abs() < f64::EPSILON);
        assert!(
            (node_volume - 0.4).abs() < f32::EPSILON,
            "the node is seeded with category × base, got {node_volume}"
        );
    }

    #[cfg(feature = "rand")]
    #[test]
    fn randomization_lands_in_the_baselines_rather_than_only_the_node() {
        let mut app = play_app();
        app.world_mut().write_message(
            PlayAudio::new(Handle::default(), TestSound::Sfx).randomized(
                Randomization::VolumeAndSpeed {
                    volume: 0.2,
                    speed: 0.2,
                },
            ),
        );
        app.update();

        let (base, pitch, speed, node_volume) = spawned(&mut app);
        assert!(
            (0.8..=1.2).contains(&base.0),
            "the drawn volume is the baseline, got {}",
            base.0
        );
        assert!(
            (0.8..=1.2).contains(&pitch.0),
            "the drawn speed is the baseline, got {}",
            pitch.0
        );
        assert!(
            (speed - f64::from(pitch.0)).abs() < 1e-9,
            "playback speed starts at the pitch baseline, so a damping field \
             has something to bend from"
        );
        assert!((node_volume - 0.8 * base.0).abs() < 1e-6);
    }

    #[test]
    fn a_hostile_request_volume_silences_rather_than_poisoning_the_mix() {
        let mut app = play_app();
        app.world_mut().write_message(
            PlayAudio::new(Handle::default(), TestSound::Sfx)
                .with_volume(f32::NAN)
                .randomized(exact()),
        );
        app.update();

        let (base, _, _, node_volume) = spawned(&mut app);
        assert_eq!(base, BaseVolume::SILENT);
        assert_eq!(node_volume, 0.0);
    }
}

// -- Category volume update tests --

mod category_volume_updates {
    use core::time::Duration;

    use bevy_seedling::prelude::*;

    use super::*;
    use crate::baseline::BaseVolume;
    use crate::fade::{FadeInAudio, FadeOutAudio};

    fn volume_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.init_resource::<TestConfig>();
        app.add_plugins(crate::MsgSeedlingPlugin::<TestSound>::default());
        // Flush the initial resource-changed run before spawning samples.
        app.update();
        app
    }

    fn spawn_sample(app: &mut App, extra: impl Bundle) -> Entity {
        app.world_mut()
            .spawn((
                TestSound::Sfx,
                sample_effects![VolumeNode::from_linear(0.4)],
                extra,
            ))
            .id()
    }

    fn effect_volume(world: &World, entity: Entity) -> f32 {
        let effects = world.get::<SampleEffects>(entity).expect("sample effects");
        let node = effects
            .iter()
            .find_map(|effect| world.get::<VolumeNode>(effect))
            .expect("volume node effect");
        node.volume.linear()
    }

    #[test]
    fn config_change_updates_steady_samples() {
        let mut app = volume_app();
        let steady = spawn_sample(&mut app, ());

        app.world_mut().resource_mut::<TestConfig>().sfx = 0.9;
        app.update();

        assert!((effect_volume(app.world(), steady) - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn config_change_leaves_fading_samples_untouched() {
        let mut app = volume_app();
        let fading_out = spawn_sample(
            &mut app,
            FadeOutAudio::new(Duration::from_secs(1)).keep_entity(),
        );
        let fading_in = spawn_sample(&mut app, FadeInAudio::new(Duration::from_secs(1), 1.0));
        let steady = spawn_sample(&mut app, ());

        app.world_mut().resource_mut::<TestConfig>().sfx = 0.9;
        app.update();

        // The fades own their volume nodes; only the steady sample follows
        // the config change.
        assert!((effect_volume(app.world(), fading_out) - 0.4).abs() < f32::EPSILON);
        assert!((effect_volume(app.world(), fading_in) - 0.4).abs() < f32::EPSILON);
        assert!((effect_volume(app.world(), steady) - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn a_config_change_keeps_each_sound_s_own_level() {
        let mut app = volume_app();
        // Two sounds of one category at different levels — a quiet ambience
        // bed under a full-volume cue, say.
        let quiet = spawn_sample(&mut app, BaseVolume(0.25));
        let full = spawn_sample(&mut app, BaseVolume(1.0));

        app.world_mut().resource_mut::<TestConfig>().sfx = 0.8;
        app.update();

        assert!(
            (effect_volume(app.world(), quiet) - 0.2).abs() < f32::EPSILON,
            "the config change scales the sound's own level, it does not replace it"
        );
        assert!((effect_volume(app.world(), full) - 0.8).abs() < f32::EPSILON);
    }

    #[test]
    fn a_sound_without_a_baseline_still_follows_the_category() {
        let mut app = volume_app();
        let bare = spawn_sample(&mut app, ());

        app.world_mut().resource_mut::<TestConfig>().sfx = 0.9;
        app.update();

        // Documented fallback: no `BaseVolume` reads as `1.0`.
        assert!((effect_volume(app.world(), bare) - 0.9).abs() < f32::EPSILON);
    }

    /// The other half of this — that a completed keep-entity fade actually
    /// lands the sound on [`BaseVolume::SILENT`] — is
    /// `fade::tests::a_completed_keep_entity_fade_lands_on_silence`.
    #[test]
    fn a_faded_out_sound_is_not_revived_by_a_config_change() {
        let mut app = volume_app();
        let faded = spawn_sample(&mut app, BaseVolume::SILENT);

        app.world_mut().resource_mut::<TestConfig>().sfx = 0.9;
        app.update();

        assert_eq!(
            effect_volume(app.world(), faded),
            0.0,
            "a volume-slider change must not bring a faded-out sound back"
        );
    }

    #[test]
    fn fade_audio_message_attaches_a_keep_entity_fade() {
        let mut app = volume_app();
        let sample = spawn_sample(&mut app, ());
        let other = app
            .world_mut()
            .spawn((
                TestSound::Music,
                sample_effects![VolumeNode::from_linear(0.4)],
            ))
            .id();

        app.world_mut()
            .write_message(FadeAudio::new(TestSound::Sfx, 1.0));
        app.update();

        let fade = app
            .world()
            .get::<FadeOutAudio>(sample)
            .expect("matching sample fades out");
        assert!(!fade.despawn_on_complete, "the entity outlives the fade");
        assert!(
            app.world().get::<FadeOutAudio>(other).is_none(),
            "other categories are left alone"
        );

        // While the fade holds the node, config rewrites stand down.
        app.world_mut().resource_mut::<TestConfig>().sfx = 0.9;
        app.update();
        assert!((effect_volume(app.world(), sample) - 0.4).abs() < f32::EPSILON);
    }
}

// -- Master volume and the mix fade --

mod master_volume_and_mix_fade {
    use bevy_seedling::prelude::*;

    use super::*;
    use crate::mix_fade::{MixFadeState, MixLevel};

    fn master_app() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.insert_resource(TestConfig {
            master: 1.0,
            ..Default::default()
        });
        app.init_resource::<MixFadeState>();
        app.add_plugins(crate::MsgSeedlingPlugin::<TestSound>::default());
        let bus = app
            .world_mut()
            .spawn((VolumeNode::from_linear(1.0), MainBus))
            .id();
        // Flush the initial resource-changed run.
        app.update();
        (app, bus)
    }

    fn bus_volume(app: &App, bus: Entity) -> f32 {
        app.world()
            .get::<VolumeNode>(bus)
            .expect("main bus volume node")
            .volume
            .linear()
    }

    #[test]
    fn a_config_change_reaches_the_master_bus_at_full() {
        let (mut app, bus) = master_app();
        app.world_mut().resource_mut::<TestConfig>().master = 0.4;
        app.update();
        assert!((bus_volume(&app, bus) - 0.4).abs() < f32::EPSILON);
    }

    #[test]
    fn a_config_change_does_not_snap_a_faded_out_mix() {
        let (mut app, bus) = master_app();
        // The mix is pointed at silence (faded out, or still fading).
        app.world_mut().resource_mut::<MixFadeState>().target = MixLevel::Silent;
        app.world_mut().resource_mut::<TestConfig>().master = 0.4;
        app.update();
        assert!(
            (bus_volume(&app, bus) - 1.0).abs() < f32::EPSILON,
            "the engaged fade owns the bus"
        );

        // Pointed back at full, config changes land again.
        app.world_mut().resource_mut::<MixFadeState>().target = MixLevel::Full;
        app.world_mut().resource_mut::<TestConfig>().set_changed();
        app.update();
        assert!((bus_volume(&app, bus) - 0.4).abs() < f32::EPSILON);
    }
}

// -- Randomization enum coverage --

#[test]
fn randomization_default_variant() {
    let r = Randomization::default();
    assert!(matches!(r, Randomization::Default));
}

#[test]
fn randomization_clone_copy() {
    let r = Randomization::VolumeAndSpeed {
        volume: 0.1,
        speed: 0.2,
    };
    let r2 = r;
    let r3 = r;
    assert!(
        matches!(r2, Randomization::VolumeAndSpeed { volume, speed } if (volume - 0.1).abs() < f32::EPSILON && (speed - 0.2).abs() < f32::EPSILON)
    );
    assert!(
        matches!(r3, Randomization::VolumeAndSpeed { volume, speed } if (volume - 0.1).abs() < f32::EPSILON && (speed - 0.2).abs() < f32::EPSILON)
    );
}

#[test]
fn randomization_debug() {
    let r = Randomization::Volume(0.5);
    let debug = format!("{r:?}");
    assert!(debug.contains("Volume"));
    assert!(debug.contains("0.5"));
}
