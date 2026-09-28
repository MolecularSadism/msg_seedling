//! An audio backend that runs the whole graph and sends its output nowhere.
//!
//! [`NullBackend`] drives Firewheel's processor from its own thread at the
//! stream's real-time rate — every sampler, bus, filter and fade processes
//! exactly as it would on a device — and discards each rendered block instead
//! of handing it to the OS. Use it where nobody should hear the game but the
//! audio still has to run: automated tests, bots, headless runs, and machines
//! with no sound card.
//!
//! Every block it discards is first summarized into a [`NullOutputMeter`], so a
//! test can assert that the graph is running and that something audible and
//! well-formed came out of it.
//!
//! ```
//! use bevy::prelude::*;
//! use bevy_seedling::prelude::*;
//! use msg_seedling::null_backend::{NullBackend, NullBackendConfig, NullOutputMeter};
//!
//! let meter = NullOutputMeter::default();
//! let mut app = App::new();
//! app.add_plugins((MinimalPlugins, AssetPlugin::default()));
//! app.insert_resource(meter.clone());
//! app.add_plugins(SeedlingPlugin::<NullBackend> {
//!     stream_config: NullBackendConfig {
//!         meter,
//!         ..default()
//!     },
//!     ..SeedlingPlugin::<NullBackend>::new()
//! });
//! app.update();
//! ```

use bevy::prelude::*;
use bevy_seedling::firewheel::{
    StreamInfo,
    backend::{AudioBackend, BackendProcessInfo, DeviceInfoSimple, SimpleStreamConfig},
    node::StreamStatus,
    processor::FirewheelProcessor,
};
use core::convert::Infallible;
use core::num::NonZeroU32;
use core::time::Duration;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::Instant;

/// Identifier the null stream reports for its one output device.
pub const NULL_DEVICE_ID: &str = "null";

/// How far the render thread may fall behind real time before it stops
/// catching up and restarts its clock from now, in blocks.
const MAX_LAG_BLOCKS: u32 = 8;

/// Stream settings for [`NullBackend`].
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

/// A Firewheel backend with no device behind it.
///
/// See the [module docs](self). Dropping it stops the render thread and hands
/// the processor back to the context, so the stream restarts cleanly.
pub struct NullBackend {
    processor_tx: Option<Sender<FirewheelProcessor<Self>>>,
    thread: Option<JoinHandle<()>>,
}

impl core::fmt::Debug for NullBackend {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("NullBackend").finish_non_exhaustive()
    }
}

impl AudioBackend for NullBackend {
    type Enumerator = ();
    type Config = NullBackendConfig;
    type StartStreamError = std::io::Error;
    type StreamError = Infallible;
    type Instant = Instant;

    fn enumerator() -> Self::Enumerator {}

    fn output_devices_simple(&mut self) -> Vec<DeviceInfoSimple> {
        vec![DeviceInfoSimple {
            name: "Null output".into(),
            id: NULL_DEVICE_ID.into(),
        }]
    }

    fn convert_simple_config(&mut self, config: &SimpleStreamConfig) -> Self::Config {
        let defaults = NullBackendConfig::default();
        let non_zero = |value: Option<u32>| value.and_then(NonZeroU32::new);
        NullBackendConfig {
            sample_rate: non_zero(config.desired_sample_rate).unwrap_or(defaults.sample_rate),
            block_frames: non_zero(config.desired_block_frames).unwrap_or(defaults.block_frames),
            channels: non_zero(config.output.channels.and_then(|c| u32::try_from(c).ok()))
                .unwrap_or(defaults.channels),
            meter: defaults.meter,
        }
    }

    fn start_stream(config: Self::Config) -> Result<(Self, StreamInfo), Self::StartStreamError> {
        let (processor_tx, processor_rx) = mpsc::channel();
        let stream_info = StreamInfo {
            sample_rate: config.sample_rate,
            prev_sample_rate: config.sample_rate,
            max_block_frames: config.block_frames,
            num_stream_in_channels: 0,
            num_stream_out_channels: config.channels.get(),
            output_device_id: NULL_DEVICE_ID.into(),
            ..default()
        };
        let thread = std::thread::Builder::new()
            .name("msg_seedling null audio".into())
            .spawn(move || render(&config, &processor_rx))?;

        Ok((
            Self {
                processor_tx: Some(processor_tx),
                thread: Some(thread),
            },
            stream_info,
        ))
    }

    fn set_processor(&mut self, processor: FirewheelProcessor<Self>) {
        if let Some(processor_tx) = &self.processor_tx {
            // The receiver only disappears once the thread has exited, which
            // happens after `drop` has taken the sender.
            let _ = processor_tx.send(processor);
        }
    }

    fn poll_status(&mut self) -> Result<(), Self::StreamError> {
        Ok(())
    }

    fn delay_from_last_process(&self, process_timestamp: Self::Instant) -> Option<Duration> {
        Some(process_timestamp.elapsed())
    }
}

impl Drop for NullBackend {
    fn drop(&mut self) {
        // Disconnecting the channel is the render thread's stop signal.
        self.processor_tx.take();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            error!("msg_seedling null audio thread panicked");
        }
    }
}

/// The render thread: one block per block-duration of wall-clock time, until
/// the backend drops its sender.
fn render(config: &NullBackendConfig, processor_rx: &Receiver<FirewheelProcessor<NullBackend>>) {
    let frames = config.block_frames.get() as usize;
    let channels = config.channels.get() as usize;
    let block_duration = Duration::from_secs_f64(
        f64::from(config.block_frames.get()) / f64::from(config.sample_rate.get()),
    );
    let mut output = vec![0.0_f32; frames * channels];
    let mut processor: Option<FirewheelProcessor<NullBackend>> = None;

    let stream_start = Instant::now();
    let mut next_block = stream_start;

    loop {
        loop {
            match processor_rx.try_recv() {
                Ok(new_processor) => processor = Some(new_processor),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        let now = Instant::now();
        let mut output_stream_status = StreamStatus::empty();
        if now > next_block + block_duration * MAX_LAG_BLOCKS {
            next_block = now;
            output_stream_status.insert(StreamStatus::OUTPUT_UNDERFLOW);
        }

        if let Some(processor) = &mut processor {
            processor.process_interleaved(
                &[],
                &mut output,
                BackendProcessInfo {
                    num_in_channels: 0,
                    num_out_channels: channels,
                    frames,
                    process_timestamp: now,
                    duration_since_stream_start: now - stream_start,
                    input_stream_status: StreamStatus::empty(),
                    output_stream_status,
                    dropped_frames: 0,
                },
            );
            config.meter.record(frames, &output);
        }

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
        event::ProcEvents,
        node::{
            AudioNode, AudioNodeInfo, AudioNodeProcessor, ConstructProcessorContext, EmptyConfig,
            ProcBuffers, ProcExtra, ProcInfo, ProcessStatus,
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

        fn info(&self, _config: &Self::Configuration) -> AudioNodeInfo {
            AudioNodeInfo::new()
                .debug_name("square test tone")
                .channel_config(ChannelConfig {
                    num_inputs: ChannelCount::ZERO,
                    num_outputs: ChannelCount::STEREO,
                })
        }

        fn construct_processor(
            &self,
            _config: &Self::Configuration,
            _cx: ConstructProcessorContext,
        ) -> impl AudioNodeProcessor {
            SquareProcessor {
                params: self.clone(),
                high: true,
            }
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
            _events: &mut ProcEvents,
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
        app.add_plugins((
            MinimalPlugins,
            msg_testing::test_asset_plugin(),
            SeedlingPlugin::<NullBackend> {
                stream_config: NullBackendConfig {
                    meter: meter.clone(),
                    ..default()
                },
                ..SeedlingPlugin::<NullBackend>::new()
            },
        ));
        app.register_simple_node::<SquareNode>();
        app.finish();
        app.cleanup();
        (app, meter)
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
    fn simple_config_maps_onto_the_null_stream() {
        let mut backend = NullBackend {
            processor_tx: None,
            thread: None,
        };
        let config = backend.convert_simple_config(&SimpleStreamConfig {
            desired_sample_rate: Some(44_100),
            desired_block_frames: Some(256),
            ..default()
        });
        assert_eq!(config.sample_rate.get(), 44_100);
        assert_eq!(config.block_frames.get(), 256);
        assert_eq!(config.channels, NullBackendConfig::default().channels);
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
            .resource_mut::<bevy_seedling::context::AudioStreamConfig<NullBackend>>()
            .set_changed();
        app.update();
        meter.reset();

        assert!(
            app.update_until(BUDGET, |_| meter.blocks() >= 4),
            "the restarted stream never rendered"
        );
    }
}
