//! An audio backend that runs the whole graph and sends its output nowhere.
//!
//! [`NullBackendPlugin`] drives Firewheel's processor from its own thread at
//! the stream's real-time rate — every sampler, bus, filter and fade processes
//! exactly as it would on a device — and discards each rendered block instead
//! of handing it to the OS. Use it where nobody should hear the game but the
//! audio still has to run: automated tests, bots, headless runs, and machines
//! with no sound card.
//!
//! It is a `bevy_seedling` platform plugin: add it in place of the `cpal`
//! one. Its stream settings are the [`AudioStreamConfig<NullBackendConfig>`]
//! resource; mutating it restarts the stream, as with any other platform.
//!
//! Every block it discards is first summarized into a [`NullOutputMeter`], so a
//! test can assert that the graph is running and that something audible and
//! well-formed came out of it.
//!
//! ```
//! use bevy::prelude::*;
//! use bevy_seedling::platform::cpal::CpalPlatformPlugin;
//! use bevy_seedling::prelude::*;
//! use msg_seedling::null_backend::{NullBackendConfig, NullBackendPlugin, NullOutputMeter};
//!
//! let meter = NullOutputMeter::default();
//! let mut app = App::new();
//! app.add_plugins((MinimalPlugins, AssetPlugin::default()));
//! app.insert_resource(meter.clone());
//! app.insert_resource(AudioStreamConfig(NullBackendConfig {
//!     meter,
//!     ..default()
//! }));
//! app.add_plugins((
//!     SeedlingPlugins.build().disable::<CpalPlatformPlugin>(),
//!     NullBackendPlugin,
//! ));
//! app.update();
//! ```

use audioadapter_buffers::direct::InterleavedSlice;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy_seedling::context::{
    AudioContext, PreStreamRestartEvent, SampleRate, StreamRestartEvent, pre_restart_stream,
};
use bevy_seedling::firewheel::{
    ActivateInfo, backend::BackendProcessInfo, node::StreamStatus, processor::FirewheelProcessor,
};
use bevy_seedling::platform::{AudioStreamConfig, RestartAudioStream, initialize_stream};
use bevy_seedling::prelude::SeedlingStartupSystems;
use bevy_seedling::resource_changed_without_insert;
use core::num::NonZeroU32;
use core::time::Duration;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;

/// How far the render thread may fall behind real time before it stops
/// catching up and restarts its clock from now, in blocks.
const MAX_LAG_BLOCKS: u32 = 8;

/// Stream settings for [`NullBackendPlugin`], held in
/// [`AudioStreamConfig<NullBackendConfig>`].
#[derive(Debug, Clone)]
pub struct NullBackendConfig {
    /// Sample rate the graph renders at.
    pub sample_rate: NonZeroU32,
    /// Frames rendered per process cycle.
    pub block_frames: NonZeroU32,
    /// Interleaved output channels.
    pub channels: NonZeroU32,
    /// Where each discarded block is summarized.
    pub meter: NullOutputMeter,
}

impl Default for NullBackendConfig {
    fn default() -> Self {
        Self {
            sample_rate: NonZeroU32::new(48_000).unwrap(),
            block_frames: NonZeroU32::new(1024).unwrap(),
            channels: NonZeroU32::new(2).unwrap(),
            meter: NullOutputMeter::default(),
        }
    }
}

/// Running summary of everything the null stream has rendered.
///
/// Cloning shares the same counters: keep one clone as a resource and pass
/// another in [`NullBackendConfig::meter`]. The render thread writes it with
/// relaxed atomics, so a read reflects some recent block, not the current
/// sample.
#[derive(Resource, Debug, Clone, Default)]
pub struct NullOutputMeter(Arc<MeterState>);

#[derive(Debug, Default)]
struct MeterState {
    blocks: AtomicU64,
    frames: AtomicU64,
    /// Bits of the largest finite absolute sample value. Non-negative `f32`
    /// bit patterns order the same as their values, so `fetch_max` on the
    /// bits tracks the float maximum.
    peak_bits: AtomicU32,
    non_finite_samples: AtomicU64,
}

impl NullOutputMeter {
    /// Process cycles rendered.
    #[must_use]
    pub fn blocks(&self) -> u64 {
        self.0.blocks.load(Ordering::Relaxed)
    }

    /// Frames rendered, per channel.
    #[must_use]
    pub fn frames(&self) -> u64 {
        self.0.frames.load(Ordering::Relaxed)
    }

    /// Largest absolute finite sample value rendered, linear (1.0 is full
    /// scale). Zero while only silence has come out.
    #[must_use]
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.0.peak_bits.load(Ordering::Relaxed))
    }

    /// Samples rendered as NaN or infinity.
    #[must_use]
    pub fn non_finite_samples(&self) -> u64 {
        self.0.non_finite_samples.load(Ordering::Relaxed)
    }

    /// Zeroes every counter, so later reads cover only what renders next.
    pub fn reset(&self) {
        self.0.blocks.store(0, Ordering::Relaxed);
        self.0.frames.store(0, Ordering::Relaxed);
        self.0.peak_bits.store(0, Ordering::Relaxed);
        self.0.non_finite_samples.store(0, Ordering::Relaxed);
    }

    fn record(&self, frames: usize, output: &[f32]) {
        let mut peak = 0.0_f32;
        let mut non_finite = 0_u64;
        for sample in output {
            if sample.is_finite() {
                peak = peak.max(sample.abs());
            } else {
                non_finite += 1;
            }
        }
        self.0.blocks.fetch_add(1, Ordering::Relaxed);
        self.0.frames.fetch_add(frames as u64, Ordering::Relaxed);
        self.0
            .peak_bits
            .fetch_max(peak.to_bits(), Ordering::Relaxed);
        if non_finite > 0 {
            self.0
                .non_finite_samples
                .fetch_add(non_finite, Ordering::Relaxed);
        }
    }
}

/// `bevy_seedling` platform plugin running the graph on a stream with no
/// device behind it.
///
/// See the [module docs](self). It starts the stream at
/// [`SeedlingStartupSystems::StreamInitialization`], restarts it whenever
/// [`AudioStreamConfig<NullBackendConfig>`] changes or
/// [`RestartAudioStream`] is triggered, and stops the render thread when the
/// app drops.
#[derive(Debug, Default)]
pub struct NullBackendPlugin;

impl Plugin for NullBackendPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AudioStreamConfig<NullBackendConfig>>()
            .add_systems(
                PostStartup,
                start_stream.in_set(SeedlingStartupSystems::StreamInitialization),
            )
            .add_systems(
                PostUpdate,
                (pre_restart_stream, restart_stream).chain().run_if(
                    resource_changed_without_insert::<AudioStreamConfig<NullBackendConfig>>,
                ),
            )
            .add_observer(observe_restart);
    }
}

/// Starts the null stream when the platform plugin's stream failed to start.
///
/// Add it next to the `cpal` platform plugin. A failed device stream leaves
/// the graph without a sample rate, so `bevy_seedling` never registers its
/// sample loader and every sample load stays pending; this plugin starts the
/// null stream in that case, with [`AudioStreamConfig<NullBackendConfig>`].
/// It drops that stream on [`PreStreamRestartEvent`], so a later restart of
/// the platform stream (a new default device, a [`RestartAudioStream`]) can
/// take the graph back.
#[derive(Debug, Default)]
pub struct NullFallbackPlugin;

impl Plugin for NullFallbackPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AudioStreamConfig<NullBackendConfig>>()
            .add_systems(
                PostStartup,
                start_fallback_stream
                    .after(SeedlingStartupSystems::StreamInitialization)
                    .run_if(not(resource_exists::<SampleRate>)),
            )
            .add_observer(release_fallback_stream);
    }
}

/// The null stream [`NullFallbackPlugin`] runs in place of a failed device;
/// held only to be dropped.
#[derive(Resource)]
struct FallbackStream {
    _stream: NullStream,
}

fn start_fallback_stream(
    mut context: ResMut<AudioContext>,
    config: Res<AudioStreamConfig<NullBackendConfig>>,
    mut commands: Commands,
) -> Result {
    warn!("no audio output stream, running the audio graph on the null backend");
    let stream = NullStream::start(&mut context, &config.0)?;
    commands.insert_resource(FallbackStream { _stream: stream });
    initialize_stream(SampleRate::new(config.0.sample_rate), commands);
    Ok(())
}

fn release_fallback_stream(_: On<PreStreamRestartEvent>, mut commands: Commands) {
    commands.remove_resource::<FallbackStream>();
}

/// The running render thread. Dropping it stops the thread, which hands the
/// processor back to the context so the stream can restart cleanly.
#[derive(Resource)]
struct NullStream {
    stop_tx: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl NullStream {
    fn start(context: &mut AudioContext, config: &NullBackendConfig) -> Result<Self, BevyError> {
        let activate = ActivateInfo {
            sample_rate: config.sample_rate,
            max_block_frames: config.block_frames,
            num_stream_in_channels: 0,
            num_stream_out_channels: config.channels.get(),
            input_to_output_latency_seconds: 0.0,
        };
        let processor = context.with(move |context| context.activate(activate))?;

        let (stop_tx, stop_rx) = mpsc::channel();
        let render_config = config.clone();
        let thread = std::thread::Builder::new()
            .name("msg_seedling null audio".into())
            .spawn(move || render(&render_config, processor, &stop_rx))?;

        Ok(Self {
            stop_tx: Some(stop_tx),
            thread: Some(thread),
        })
    }
}

impl Drop for NullStream {
    fn drop(&mut self) {
        // Disconnecting the channel is the render thread's stop signal.
        self.stop_tx.take();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            error!("msg_seedling null audio thread panicked");
        }
    }
}

fn start_stream(
    mut context: ResMut<AudioContext>,
    config: Res<AudioStreamConfig<NullBackendConfig>>,
    mut commands: Commands,
) -> Result {
    let stream = NullStream::start(&mut context, &config.0)?;
    commands.insert_resource(stream);
    initialize_stream(SampleRate::new(config.0.sample_rate), commands);
    Ok(())
}

fn restart_stream(world: &mut World) -> Result {
    // The old thread has to hand its processor back before the context can
    // activate again.
    drop(world.remove_resource::<NullStream>());

    let config = world
        .resource::<AudioStreamConfig<NullBackendConfig>>()
        .0
        .clone();
    let stream = NullStream::start(&mut world.resource_mut::<AudioContext>(), &config)?;
    world.insert_resource(stream);

    let current_rate = config.sample_rate;
    let previous_rate = world.resource::<SampleRate>().get();
    world.resource::<SampleRate>().set(current_rate);
    world.trigger(StreamRestartEvent {
        previous_rate,
        current_rate,
    });
    Ok(())
}

fn observe_restart(
    _: On<RestartAudioStream>,
    mut config: ResMut<AudioStreamConfig<NullBackendConfig>>,
) {
    config.set_changed();
}

/// The render thread: one block per block-duration of wall-clock time, until
/// the stream drops its sender.
fn render(config: &NullBackendConfig, mut processor: FirewheelProcessor, stop_rx: &Receiver<()>) {
    let frames = config.block_frames.get() as usize;
    let channels = config.channels.get() as usize;
    let block_duration = Duration::from_secs_f64(
        f64::from(config.block_frames.get()) / f64::from(config.sample_rate.get()),
    );
    let mut output = vec![0.0_f32; frames * channels];

    let stream_start = Instant::now();
    let mut next_block = stream_start;

    while let Err(TryRecvError::Empty) = stop_rx.try_recv() {
        let now = Instant::now();
        let mut output_stream_status = StreamStatus::empty();
        if now > next_block + block_duration * MAX_LAG_BLOCKS {
            next_block = now;
            output_stream_status.insert(StreamStatus::OUTPUT_UNDERFLOW);
        }

        processor.process(
            &InterleavedSlice::new(&[], 0, frames).expect("an empty input fits any block"),
            &mut InterleavedSlice::new_mut(&mut output, channels, frames)
                .expect("the output buffer holds one block"),
            BackendProcessInfo {
                frames,
                process_timestamp: Some(now),
                duration_since_stream_start: now - stream_start,
                input_stream_status: StreamStatus::empty(),
                output_stream_status,
                dropped_frames: 0,
                process_to_playback_delay: None,
            },
        );
        config.meter.record(frames, &output);

        next_block += block_duration;
        if let Some(wait) = next_block.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_seedling::firewheel::{
        channel_config::{ChannelConfig, ChannelCount},
        node::{
            AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
            NodeError, ProcBuffers, ProcExtra, ProcInfo, ProcessStatus,
        },
    };
    use bevy_seedling::prelude::*;
    use msg_testing::AppTesting;

    const BUDGET: Duration = Duration::from_secs(10);

    /// A constant-amplitude square wave, so the expected peak is exact.
    #[derive(Debug, Clone, Component)]
    struct SquareNode {
        amplitude: f32,
    }

    impl AudioNode for SquareNode {
        type Configuration = EmptyConfig;

        fn info(&self, _config: &Self::Configuration) -> Result<AudioNodeInfo, NodeError> {
            Ok(AudioNodeInfo::new()
                .debug_name("square test tone")
                .channel_config(ChannelConfig {
                    num_inputs: ChannelCount::ZERO,
                    num_outputs: ChannelCount::STEREO,
                }))
        }

        fn construct_processor(
            &self,
            _config: &Self::Configuration,
            _cx: ConstructProcessorContext,
        ) -> Result<impl AudioNodeProcessor, NodeError> {
            Ok(SquareProcessor {
                params: self.clone(),
                high: true,
            })
        }
    }

    struct SquareProcessor {
        params: SquareNode,
        high: bool,
    }

    impl AudioNodeProcessor for SquareProcessor {
        fn process(
            &mut self,
            _info: &ProcInfo,
            ProcBuffers { outputs, .. }: ProcBuffers,
            _extra: &mut ProcExtra,
        ) -> ProcessStatus {
            let frames = outputs.first().map_or(0, |channel| channel.len());
            for frame in 0..frames {
                let sample = if self.high {
                    self.params.amplitude
                } else {
                    -self.params.amplitude
                };
                self.high = !self.high;
                for channel in outputs.iter_mut() {
                    channel[frame] = sample;
                }
            }
            ProcessStatus::OutputsModified
        }
    }

    fn null_app() -> (App, NullOutputMeter) {
        let meter = NullOutputMeter::default();
        let mut app = App::new();
        app.insert_resource(AudioStreamConfig(NullBackendConfig {
            meter: meter.clone(),
            ..default()
        }));
        app.add_plugins((
            MinimalPlugins,
            msg_testing::test_asset_plugin(),
            SeedlingPlugins
                .build()
                .disable::<bevy_seedling::platform::cpal::CpalPlatformPlugin>(),
            NullBackendPlugin,
        ));
        app.register_simple_node::<SquareNode>();
        app.finish();
        app.cleanup();
        (app, meter)
    }

    /// An app whose platform stream never starts, with the fallback beside it.
    fn fallback_app() -> (App, NullOutputMeter) {
        let meter = NullOutputMeter::default();
        let mut app = App::new();
        app.insert_resource(AudioStreamConfig(NullBackendConfig {
            meter: meter.clone(),
            ..default()
        }));
        app.add_plugins((
            MinimalPlugins,
            msg_testing::test_asset_plugin(),
            SeedlingPlugins
                .build()
                .disable::<bevy_seedling::platform::cpal::CpalPlatformPlugin>(),
            NullFallbackPlugin,
        ));
        app.finish();
        app.cleanup();
        (app, meter)
    }

    #[test]
    fn a_failed_platform_stream_falls_back_to_the_null_stream() {
        let (mut app, meter) = fallback_app();

        assert!(
            app.update_until(BUDGET, |_| meter.blocks() > 0),
            "the fallback stream never rendered"
        );
        assert!(app.world().contains_resource::<FallbackStream>());
        assert!(app.world().contains_resource::<SampleRate>());
    }

    #[test]
    fn a_running_platform_stream_keeps_the_fallback_idle() {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            msg_testing::test_asset_plugin(),
            SeedlingPlugins
                .build()
                .disable::<bevy_seedling::platform::cpal::CpalPlatformPlugin>(),
            NullBackendPlugin,
            NullFallbackPlugin,
        ));
        app.finish();
        app.cleanup();
        app.update();

        assert!(app.world().contains_resource::<NullStream>());
        assert!(!app.world().contains_resource::<FallbackStream>());
    }

    #[test]
    fn a_stream_restart_releases_the_fallback() {
        let (mut app, meter) = fallback_app();
        assert!(app.update_until(BUDGET, |_| meter.blocks() > 0));

        app.world_mut().trigger(PreStreamRestartEvent);
        app.world_mut().flush();

        assert!(!app.world().contains_resource::<FallbackStream>());
    }

    #[test]
    fn meter_tracks_peak_and_non_finite_samples() {
        let meter = NullOutputMeter::default();
        meter.record(2, &[0.25, -0.5, f32::NAN, 0.1]);
        meter.record(1, &[f32::INFINITY, -0.75]);

        assert_eq!(meter.blocks(), 2);
        assert_eq!(meter.frames(), 3);
        assert!((meter.peak() - 0.75).abs() < f32::EPSILON);
        assert_eq!(meter.non_finite_samples(), 2);

        meter.reset();
        assert_eq!(meter.blocks(), 0);
        assert_eq!(meter.frames(), 0);
        assert!(meter.peak().abs() < f32::EPSILON);
        assert_eq!(meter.non_finite_samples(), 0);
    }

    #[test]
    fn a_changed_config_restarts_at_its_sample_rate() {
        let (mut app, meter) = null_app();
        assert!(app.update_until(BUDGET, |_| meter.blocks() > 0));

        let rate = NonZeroU32::new(44_100).unwrap();
        app.world_mut()
            .resource_mut::<AudioStreamConfig<NullBackendConfig>>()
            .0
            .sample_rate = rate;
        app.update();

        assert_eq!(app.world().resource::<SampleRate>().get(), rate);
        let stream_rate = app
            .world_mut()
            .resource_mut::<AudioContext>()
            .with(|context| context.stream_info().map(|info| info.sample_rate));
        assert_eq!(stream_rate, Some(rate));
    }

    #[test]
    fn an_idle_graph_renders_silence() {
        let (mut app, meter) = null_app();

        assert!(
            app.update_until(BUDGET, |_| meter.blocks() >= 4),
            "the null stream never rendered a block"
        );
        assert!(
            meter.peak().abs() < f32::EPSILON,
            "silence expected, peak {}",
            meter.peak()
        );
        assert_eq!(meter.non_finite_samples(), 0);
    }

    #[test]
    fn a_node_on_the_main_bus_reaches_the_output() {
        let (mut app, meter) = null_app();
        let amplitude = 0.5;
        app.world_mut().spawn(SquareNode { amplitude });

        assert!(
            app.update_until(BUDGET, |_| meter.peak() > 0.0),
            "the tone never reached the null output"
        );
        assert!(
            meter.peak() <= amplitude + f32::EPSILON,
            "peak {} exceeds the tone's amplitude {amplitude}",
            meter.peak()
        );
        assert_eq!(meter.non_finite_samples(), 0);
    }

    #[test]
    fn restarting_the_stream_keeps_rendering() {
        let (mut app, meter) = null_app();
        assert!(app.update_until(BUDGET, |_| meter.blocks() > 0));

        app.world_mut()
            .resource_mut::<AudioStreamConfig<NullBackendConfig>>()
            .set_changed();
        app.update();
        meter.reset();

        assert!(
            app.update_until(BUDGET, |_| meter.blocks() >= 4),
            "the restarted stream never rendered"
        );
    }
}
