//! Detection of Bevy's own audio stack running alongside `bevy_seedling`.
//!
//! Bevy's `bevy_audio` feature is pulled in by its `2d`, `3d` and `ui` feature
//! groups, so an app that never asked for it still gets `AudioPlugin` inside
//! `DefaultPlugins`. Nothing in this crate goes through it — every sound is a
//! `bevy_seedling` sample — but the two stacks each open their own OS output
//! stream and register competing loaders for `ogg`/`wav`/`mp3`, which is how
//! seedling's stream creation intermittently fails on Windows.
//!
//! Whether `bevy_audio` is on is invisible here at compile time: cargo unifies
//! features across the whole graph, so it is a property of the final binary
//! rather than of this crate's own dependency declaration. The check is
//! therefore a runtime one, reading the only trace `AudioPlugin` always leaves:
//! it calls `init_asset` for its `AudioSource` and `Pitch` sources, and
//! `init_asset` registers `Handle<A>` in the type registry. A `TypePath` is
//! present regardless of bevy's `debug` feature, unlike a `DebugName`.

use bevy::prelude::*;
use bevy::reflect::TypeRegistry;

/// Type-path fragment shared by every type `bevy_audio` registers.
const BEVY_AUDIO_TYPE_PREFIX: &str = "bevy_audio::";

/// Warns once at startup when Bevy's `AudioPlugin` built into the same app.
///
/// Added automatically by [`MsgSeedlingPlugin`](crate::MsgSeedlingPlugin); add
/// it directly only when using this crate's systems without that plugin.
pub struct BevyAudioGuardPlugin;

impl Plugin for BevyAudioGuardPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, warn_on_bevy_audio);
    }
}

/// Emits the warning when the app carries a live `bevy_audio` registration.
///
/// Runs in `Startup`, by which point every plugin has built.
fn warn_on_bevy_audio(type_registry: Option<Res<AppTypeRegistry>>) {
    let Some(type_registry) = type_registry else {
        return;
    };
    if !bevy_audio_is_active(&type_registry.read()) {
        return;
    }

    warn!(
        "Bevy's `AudioPlugin` is active alongside `bevy_seedling`, and every sound in this app \
         plays through `bevy_seedling` — Bevy's audio stack is overridden and only costs a second \
         OS output stream (which can stop seedling's own stream from opening) plus a competing \
         asset loader for `ogg`/`wav`/`mp3`. Turn bevy's `bevy_audio` feature off, or, while it \
         stays on, build `DefaultPlugins` with `.disable::<bevy::audio::AudioPlugin>()`."
    );
}

/// Whether any `bevy_audio` type reached the registry, i.e. `AudioPlugin` built.
fn bevy_audio_is_active(registry: &TypeRegistry) -> bool {
    registers_bevy_audio(
        registry
            .iter()
            .map(|registration| registration.type_info().type_path()),
    )
}

/// Whether any of `type_paths` names a `bevy_audio` type.
fn registers_bevy_audio<'a>(mut type_paths: impl Iterator<Item = &'a str>) -> bool {
    type_paths.any(|path| path.contains(BEVY_AUDIO_TYPE_PREFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_bevy_type_paths_are_not_bevy_audio() {
        assert!(!registers_bevy_audio(
            [
                "bevy_transform::components::transform::Transform",
                "bevy_asset::handle::Handle<bevy_seedling::sample::AudioSample>",
            ]
            .into_iter()
        ));
    }

    #[test]
    fn an_audio_plugin_handle_registration_is_detected() {
        assert!(registers_bevy_audio(
            [
                "bevy_transform::components::transform::Transform",
                "bevy_asset::handle::Handle<bevy_audio::audio_source::AudioSource>",
            ]
            .into_iter()
        ));
    }

    #[test]
    fn no_type_paths_means_no_bevy_audio() {
        assert!(!registers_bevy_audio(core::iter::empty()));
    }

    #[test]
    fn a_seedling_only_app_stays_quiet() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), BevyAudioGuardPlugin));
        app.update();

        let type_registry = app.world().resource::<AppTypeRegistry>().clone();
        assert!(!bevy_audio_is_active(&type_registry.read()));
    }
}
