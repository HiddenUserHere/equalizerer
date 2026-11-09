//! The WASAPI real-time pipeline: loopback capture from the virtual device, EQ, and
//! event-driven render to the target device.
//!
//! [`WasapiPipelineFactory::start`] spawns one `audio-rt` thread. That thread owns every COM
//! object of the pipeline, runs under MMCSS and FTZ/DAZ, and reports its setup result through a
//! channel. The controller thread keeps only the parameter writer, the statistics and the stop
//! event. Architecture: `docs/architecture.md` §5.1 and §5.4.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::slice;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Sender, bounded};
use eq_core::{
    DeviceEvent, DeviceId, LatencyMode, PatternLocator, Pipeline, PipelineConfig, PipelineError,
    PipelineFactory, Stats,
};
use eq_dsp::EqParams;
use eq_engine::{
    DenormalGuard, EngineStats, ParamsReader, ParamsWriter, Processor, ProcessorConfig,
    SharedStats, StereoFifo, params_channel,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioCaptureClient, IAudioClient, IAudioClient3,
    IAudioRenderClient, IMMDevice, IMMDeviceEnumerator, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
use windows::Win32::System::Com::{CLSCTX_ALL, CoTaskMemFree};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, SetEvent,
    WaitForMultipleObjects,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::com::ComApartment;
use crate::devices::MmDirectory;
use crate::error::{ComResultExt, HRESULT_NOT_FOUND, WasapiError};
use crate::format::{
    FloatFormat, capture_format, check_float32, fifo_target_frames, frames_to_ms, hns_to_frames,
    hns_to_ms,
};

/// Channels of the capture stream and of the FIFO: loopback is always stereo.
const STEREO: usize = 2;
/// Flag value that tells the engine a render buffer holds silence.
const SILENT_FLAG: u32 = AUDCLNT_BUFFERFLAGS_SILENT.0.cast_unsigned();
/// Loopback capture buffer length in hundred-nanosecond units (200 ms).
const CAPTURE_BUFFER_HNS: i64 = 2_000_000;
/// How long one wait for the render event may block, in milliseconds. The loop wakes at this
/// rate even without audio, so a stalled device cannot hide the stop request for long.
const WAIT_TIMEOUT_MS: u32 = 2_000;

/// A Win32 event handle that is closed when dropped.
pub(crate) struct OwnedEvent(HANDLE);

// SAFETY: an event handle is not bound to a thread. SetEvent, the wait functions and CloseHandle
// may each be called from any thread, and this type is only ever used from one thread at a time.
unsafe impl Send for OwnedEvent {}

impl OwnedEvent {
    /// Creates an unnamed, unsignalled event. A manual-reset event stays signalled until it is
    /// reset; an auto-reset event releases one waiter per signal.
    fn new(manual_reset: bool) -> Result<Self, WasapiError> {
        // SAFETY: null security attributes and a null name are valid for CreateEventW. The
        // returned handle is owned by the result and closed exactly once in `Drop`.
        let handle = unsafe { CreateEventW(None, manual_reset, false, PCWSTR::null()) }
            .context("CreateEventW")?;
        Ok(Self(handle))
    }

    /// Returns the raw handle for wait and set calls. The handle stays owned by `self`.
    fn raw(&self) -> HANDLE {
        self.0
    }

    /// Returns a non-owning copy for the audio thread.
    fn non_owning(&self) -> EventRef {
        EventRef(self.0)
    }

    /// Signals the event.
    fn set(&self) -> Result<(), WasapiError> {
        // SAFETY: the handle is valid for as long as `self` lives.
        unsafe { SetEvent(self.0) }.context("SetEvent")
    }
}

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        // SAFETY: the handle was created by CreateEventW and is closed only here. The result is
        // ignored because a destructor cannot report it.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// A non-owning copy of an event handle, moved to the audio thread.
#[derive(Clone, Copy)]
struct EventRef(HANDLE);

// SAFETY: the owner is `OwnedEvent`, held by `WasapiPipeline`. `WasapiPipeline::shutdown` joins
// the audio thread before that owner is dropped, so the thread never uses a closed handle.
unsafe impl Send for EventRef {}

/// Keeps the audio thread registered with MMCSS under the "Pro Audio" task until dropped.
struct MmcssGuard(HANDLE);

impl MmcssGuard {
    /// Registers the current thread. Returns `None` when MMCSS is unavailable. MMCSS only
    /// changes thread priority, so a failure is not fatal and the pipeline runs without it.
    fn enter() -> Option<Self> {
        let mut task_index = 0_u32;
        // SAFETY: the task name is a static NUL-terminated string, and `task_index` is a valid
        // out pointer for the duration of the call.
        unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &raw mut task_index) }
            .ok()
            .map(Self)
    }
}

impl Drop for MmcssGuard {
    fn drop(&mut self) {
        // SAFETY: the handle came from AvSetMmThreadCharacteristicsW on this thread and is reverted
        // exactly once. The result is ignored because a destructor cannot report it.
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.0) };
    }
}

/// A `WAVEFORMATEX` allocated by the audio engine, freed with `CoTaskMemFree` on drop.
struct EngineFormat(NonNull<WAVEFORMATEX>);

impl EngineFormat {
    /// Takes ownership of a format returned by `GetMixFormat`.
    fn new(ptr: *mut WAVEFORMATEX) -> Result<Self, WasapiError> {
        NonNull::new(ptr).map(Self).ok_or_else(|| {
            WasapiError::UnsupportedFormat("the engine returned no mix format".to_owned())
        })
    }

    fn as_ptr(&self) -> *const WAVEFORMATEX {
        self.0.as_ptr()
    }

    /// Reads the mix format and checks that it is 32-bit float.
    fn float_format(&self) -> Result<FloatFormat, WasapiError> {
        let ptr = self.as_ptr();
        // SAFETY: `ptr` points to a WAVEFORMATEX that this value owns until drop. The header is
        // read by value, so no reference to the packed struct is created.
        let base = unsafe { ptr::read_unaligned(ptr) };
        let extensible =
            if u32::from(base.wFormatTag) == WAVE_FORMAT_EXTENSIBLE && base.cbSize >= 22 {
                // SAFETY: the tag and `cbSize` say the engine allocated the extension after the
                // header, so the bytes are a WAVEFORMATEXTENSIBLE. It is read by value.
                Some(unsafe { ptr::read_unaligned(ptr.cast::<WAVEFORMATEXTENSIBLE>()) })
            } else {
                None
            };
        check_float32(&base, extensible.as_ref()).map_err(WasapiError::UnsupportedFormat)
    }
}

impl Drop for EngineFormat {
    fn drop(&mut self) {
        // SAFETY: the pointer was allocated by the engine with the COM task allocator and is
        // freed exactly once, here.
        unsafe { CoTaskMemFree(Some(self.0.as_ptr().cast_const().cast::<c_void>())) };
    }
}

/// Finds an endpoint by its id. An unknown id becomes [`WasapiError::DeviceNotFound`].
fn device_by_id(enumerator: &IMMDeviceEnumerator, id: &DeviceId) -> Result<IMMDevice, WasapiError> {
    let wide = HSTRING::from(id.as_str());
    // SAFETY: `wide` outlives the call and the enumerator is a live object on this thread.
    unsafe { enumerator.GetDevice(&wide) }.map_err(|source| {
        if source.code() == HRESULT_NOT_FOUND {
            WasapiError::DeviceNotFound(id.to_string())
        } else {
            WasapiError::Com {
                context: "IMMDeviceEnumerator::GetDevice",
                source,
            }
        }
    })
}

/// The render half: a shared-mode, event-driven stream on the target device.
struct Render {
    client: IAudioClient3,
    writer: IAudioRenderClient,
    event: OwnedEvent,
    /// Buffer length in frames.
    buffer_frames: u32,
    /// Channels of the device mix format.
    channels: usize,
    /// Sample rate of the device mix format.
    sample_rate: u32,
    /// Engine period in frames; sets the lower-latency FIFO target.
    period_frames: usize,
    /// Engine-side latency reported by `GetStreamLatency`.
    stream_latency_ms: f32,
}

impl Render {
    /// Opens the target device with the period that `latency` asks for.
    ///
    /// Lower latency uses the smallest engine period (`IAudioClient3`); the other modes use the
    /// engine default period.
    fn open(device: &IMMDevice, latency: LatencyMode) -> Result<Self, WasapiError> {
        // SAFETY: activating an audio client with no activation parameters is the documented
        // call. The COM apartment of this thread is initialised by the caller.
        let client: IAudioClient3 = unsafe { device.Activate(CLSCTX_ALL, None) }
            .context("IMMDevice::Activate(IAudioClient3)")?;
        // SAFETY: plain query on the client created above.
        let raw_format = unsafe { client.GetMixFormat() }.context("IAudioClient::GetMixFormat")?;
        let mix = EngineFormat::new(raw_format)?;
        let format = mix.float_format()?;
        let period_frames = Self::initialize(&client, &mix, latency, format.sample_rate)?;

        let event = OwnedEvent::new(false)?;
        // SAFETY: the event is a valid auto-reset event that outlives the client, because the
        // result owns both.
        unsafe { client.SetEventHandle(event.raw()) }.context("IAudioClient::SetEventHandle")?;
        // SAFETY: plain query on an initialised client.
        let buffer_frames =
            unsafe { client.GetBufferSize() }.context("IAudioClient::GetBufferSize")?;
        // SAFETY: the service is only requested after Initialize, as the API requires.
        let render: IAudioRenderClient = unsafe { client.GetService() }
            .context("IAudioClient::GetService(IAudioRenderClient)")?;
        // SAFETY: plain query on an initialised client.
        let latency_hns =
            unsafe { client.GetStreamLatency() }.context("IAudioClient::GetStreamLatency")?;

        Ok(Self {
            client,
            writer: render,
            event,
            buffer_frames,
            channels: usize::from(format.channels),
            sample_rate: format.sample_rate,
            period_frames,
            stream_latency_ms: hns_to_ms(latency_hns),
        })
    }

    /// Initializes the client for `latency` and returns the engine period in frames.
    fn initialize(
        client: &IAudioClient3,
        mix: &EngineFormat,
        latency: LatencyMode,
        sample_rate: u32,
    ) -> Result<usize, WasapiError> {
        match latency {
            LatencyMode::Low => {
                let mut default_period = 0_u32;
                let mut fundamental_period = 0_u32;
                let mut min_period = 0_u32;
                let mut max_period = 0_u32;
                // SAFETY: `mix` is a valid format for the call, and every out pointer is a
                // local that is valid for the duration of the call.
                unsafe {
                    client.GetSharedModeEnginePeriod(
                        mix.as_ptr(),
                        &raw mut default_period,
                        &raw mut fundamental_period,
                        &raw mut min_period,
                        &raw mut max_period,
                    )
                }
                .context("IAudioClient3::GetSharedModeEnginePeriod")?;
                // SAFETY: `mix` is valid for the call, and the minimum period is one the engine
                // reported for this format.
                unsafe {
                    client.InitializeSharedAudioStream(
                        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                        min_period,
                        mix.as_ptr(),
                        None,
                    )
                }
                .context("IAudioClient3::InitializeSharedAudioStream")?;
                Ok(usize::try_from(min_period).unwrap_or(usize::MAX))
            }
            LatencyMode::Auto | LatencyMode::Reliable => {
                let mut default_hns = 0_i64;
                // SAFETY: the out pointer is a local valid for the call.
                unsafe { client.GetDevicePeriod(Some(ptr::from_mut(&mut default_hns)), None) }
                    .context("IAudioClient::GetDevicePeriod")?;
                // SAFETY: `mix` is valid for the call. A zero buffer duration selects the engine
                // default for event-driven shared mode.
                unsafe {
                    client.Initialize(
                        AUDCLNT_SHAREMODE_SHARED,
                        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                        0,
                        0,
                        mix.as_ptr(),
                        None,
                    )
                }
                .context("IAudioClient::Initialize(render)")?;
                Ok(hns_to_frames(default_hns, sample_rate))
            }
        }
    }
}

/// The capture half: a shared-mode loopback stream on the virtual device.
struct Capture {
    client: IAudioClient,
    reader: IAudioCaptureClient,
    /// Capture-side latency reported by `GetStreamLatency`.
    stream_latency_ms: f32,
}

impl Capture {
    /// Opens loopback capture on `device` as float32 stereo at `sample_rate`. Windows converts
    /// the virtual device's format and rate, so the pipeline only corrects clock drift.
    fn open(device: &IMMDevice, sample_rate: u32) -> Result<Self, WasapiError> {
        // SAFETY: activating an audio client with no activation parameters is the documented
        // call.
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
            .context("IMMDevice::Activate(IAudioClient, virtual)")?;
        let format = capture_format(sample_rate);
        let header = ptr::from_ref(&format).cast::<WAVEFORMATEX>();
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        // SAFETY: `header` points to `format`, a local that outlives the call. The extensible
        // format starts with its WAVEFORMATEX header, and both structs are packed to one byte.
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                CAPTURE_BUFFER_HNS,
                0,
                header,
                None,
            )
        }
        .context("IAudioClient::Initialize(loopback)")?;
        // SAFETY: the service is requested after Initialize, as the API requires.
        let capture: IAudioCaptureClient = unsafe { client.GetService() }
            .context("IAudioClient::GetService(IAudioCaptureClient)")?;
        // SAFETY: plain query on an initialised client.
        let latency_hns =
            unsafe { client.GetStreamLatency() }.context("IAudioClient::GetStreamLatency")?;
        Ok(Self {
            client,
            reader: capture,
            stream_latency_ms: hns_to_ms(latency_hns),
        })
    }
}

/// Outcome of one wait on the render and stop events.
enum Wake {
    /// The render engine needs a period.
    Render,
    /// The stop event was signalled.
    Stop,
    /// Nothing happened within the timeout.
    Timeout,
}

/// Everything the audio thread keeps for the whole run.
struct Session {
    render: Render,
    capture: Capture,
    fifo: StereoFifo,
    processor: Processor,
    stop: EventRef,
    /// Latency of both streams, added to the FIFO and padding delay in every period.
    static_latency_ms: f32,
}

impl Session {
    /// Builds and starts both streams. Runs on the audio thread.
    fn prepare(
        com: &ComApartment,
        route: &Route,
        params: ParamsReader,
        stats: Arc<SharedStats>,
        stop: EventRef,
    ) -> Result<Self, WasapiError> {
        let directory = MmDirectory::new(com, PatternLocator::default())?;
        let enumerator = directory.enumerator();
        let target = device_by_id(enumerator, &route.target_id)?;
        let virtual_device = device_by_id(enumerator, &route.virtual_id)?;

        let render = Render::open(&target, route.latency)?;
        let capture = Capture::open(&virtual_device, render.sample_rate)?;

        let static_latency_ms = render.stream_latency_ms + capture.stream_latency_ms;
        let fifo_target =
            fifo_target_frames(route.latency, render.period_frames, render.sample_rate);
        let mut processor = Processor::new(
            ProcessorConfig {
                sample_rate: f64::from(render.sample_rate),
                out_channels: render.channels,
                fifo_target_frames: fifo_target,
                max_block_frames: render.buffer_frames as usize,
            },
            params,
            stats,
        )?;
        processor.set_device_latency_ms(static_latency_ms);
        let fifo = StereoFifo::with_capacity_frames(render.sample_rate as usize);

        render.prefill_silence()?;
        // SAFETY: the clients are initialised. Start has no pointer arguments.
        unsafe { capture.client.Start() }.context("IAudioClient::Start(capture)")?;
        // SAFETY: as above. If the render stream fails to start, the capture stream is stopped
        // before the error is returned.
        if let Err(err) = unsafe { render.client.Start() } {
            // SAFETY: the capture client was started above and is stopped once here.
            let _ = unsafe { capture.client.Stop() };
            return Err(WasapiError::Com {
                context: "IAudioClient::Start(render)",
                source: err,
            });
        }

        Ok(Self {
            render,
            capture,
            fifo,
            processor,
            stop,
            static_latency_ms,
        })
    }

    /// Runs the render loop until the stop event fires or a WASAPI call fails.
    fn run(&mut self) -> Result<(), WasapiError> {
        loop {
            match self.wait()? {
                Wake::Stop => return Ok(()),
                Wake::Timeout => {}
                Wake::Render => self.period()?,
            }
        }
    }

    /// Waits for the render event or the stop event.
    fn wait(&self) -> Result<Wake, WasapiError> {
        let handles = [self.render.event.raw(), self.stop.0];
        // SAFETY: both handles are valid. The render event is owned by `self`, and the stop
        // event is owned by the pipeline handle, which joins this thread before closing it.
        let result = unsafe { WaitForMultipleObjects(&handles, false, WAIT_TIMEOUT_MS) };
        if result == WAIT_TIMEOUT {
            return Ok(Wake::Timeout);
        }
        match result.0.wrapping_sub(WAIT_OBJECT_0.0) {
            0 => Ok(Wake::Render),
            1 => Ok(Wake::Stop),
            _ => Err(WasapiError::Com {
                context: "WaitForMultipleObjects",
                source: windows::core::Error::from_thread(),
            }),
        }
    }

    /// One render period: move captured audio into the FIFO, then fill the render buffer.
    fn period(&mut self) -> Result<(), WasapiError> {
        self.drain_capture()?;
        self.render_period()
    }

    /// Moves every available loopback packet into the FIFO.
    fn drain_capture(&mut self) -> Result<(), WasapiError> {
        loop {
            // SAFETY: plain query on the started capture client.
            let packet = unsafe { self.capture.reader.GetNextPacketSize() }
                .context("IAudioCaptureClient::GetNextPacketSize")?;
            if packet == 0 {
                return Ok(());
            }
            let mut data = ptr::null_mut::<u8>();
            let mut frames = 0_u32;
            let mut flags = 0_u32;
            // SAFETY: the out pointers are locals that stay valid for the call. The engine
            // returns a buffer of `frames` frames that stays valid until ReleaseBuffer.
            unsafe {
                self.capture.reader.GetBuffer(
                    &raw mut data,
                    &raw mut frames,
                    &raw mut flags,
                    None,
                    None,
                )
            }
            .context("IAudioCaptureClient::GetBuffer")?;
            self.push_capture(data, frames, flags);
            // SAFETY: releases the buffer obtained above, exactly once.
            unsafe { self.capture.reader.ReleaseBuffer(frames) }
                .context("IAudioCaptureClient::ReleaseBuffer")?;
        }
    }

    /// Stores one captured packet in the FIFO. Silent packets become zeros.
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "WASAPI buffers hold whole float32 frames and are aligned to the frame size"
    )]
    fn push_capture(&mut self, data: *const u8, frames: u32, flags: u32) {
        let frames = frames as usize;
        if flags & SILENT_FLAG != 0 {
            let _ = self.fifo.pad_silence(frames);
        } else if frames > 0 {
            // SAFETY: the capture format is fixed to float32 stereo, so `frames * 2` samples are
            // readable at `data` until the caller releases the buffer. The slice is only read.
            let captured = unsafe { slice::from_raw_parts(data.cast::<f32>(), frames * STEREO) };
            let _ = self.fifo.push(captured);
        }
    }

    /// Fills the free part of the render buffer with processed audio.
    #[allow(
        clippy::cast_ptr_alignment,
        reason = "WASAPI buffers hold whole float32 frames and are aligned to the frame size"
    )]
    fn render_period(&mut self) -> Result<(), WasapiError> {
        // SAFETY: plain query on the started render client.
        let padding = unsafe { self.render.client.GetCurrentPadding() }
            .context("IAudioClient::GetCurrentPadding")?;
        let avail = self.render.buffer_frames.saturating_sub(padding);
        if avail == 0 {
            return Ok(());
        }
        // Samples written now play after the frames already queued, so the queued frames count
        // as render-side delay.
        self.processor.set_device_latency_ms(
            self.static_latency_ms + frames_to_ms(padding, self.render.sample_rate),
        );
        // SAFETY: GetBuffer grants `avail` frames of `channels` float32 samples until
        // ReleaseBuffer. The slice is used only before that call.
        let data = unsafe { self.render.writer.GetBuffer(avail) }
            .context("IAudioRenderClient::GetBuffer")?;
        // SAFETY: `data` holds `avail * channels` float32 samples, as the render format
        // was validated as float32 with that channel count. The slice is used before the buffer
        // is released below.
        let out = unsafe {
            slice::from_raw_parts_mut(data.cast::<f32>(), avail as usize * self.render.channels)
        };
        self.processor.render(&mut self.fifo, out);
        // SAFETY: releases the buffer obtained above, exactly once. The slice is not used after.
        unsafe { self.render.writer.ReleaseBuffer(avail, 0) }
            .context("IAudioRenderClient::ReleaseBuffer")?;
        Ok(())
    }

    /// Stops both streams. Errors are ignored because the streams are torn down either way.
    fn stop_clients(&self) {
        // SAFETY: Stop has no pointer arguments and is safe to call on a stopped client.
        let _ = unsafe { self.capture.client.Stop() };
        // SAFETY: as above.
        let _ = unsafe { self.render.client.Stop() };
    }
}

impl Render {
    /// Fills the render buffer with silence so that the first period starts without a gap.
    fn prefill_silence(&self) -> Result<(), WasapiError> {
        let frames = self.buffer_frames;
        // SAFETY: GetBuffer grants the whole buffer until ReleaseBuffer, and the silent flag
        // tells the engine not to play the bytes, so whatever they contain is never heard.
        unsafe { self.writer.GetBuffer(frames) }
            .context("IAudioRenderClient::GetBuffer(prefill)")?;
        // SAFETY: releases the buffer obtained above, exactly once.
        unsafe { self.writer.ReleaseBuffer(frames, SILENT_FLAG) }
            .context("IAudioRenderClient::ReleaseBuffer(prefill)")
    }
}

/// The endpoints and mode a pipeline runs with.
struct Route {
    virtual_id: DeviceId,
    target_id: DeviceId,
    latency: LatencyMode,
}

/// Everything the audio thread receives from [`WasapiPipelineFactory::start`].
struct ThreadArgs {
    route: Route,
    params: ParamsReader,
    stats: Arc<SharedStats>,
    stop: EventRef,
    failures: Sender<DeviceEvent>,
    init: Sender<Result<(), WasapiError>>,
}

/// Body of the `audio-rt` thread: setup, the render loop, and teardown.
fn audio_thread(args: ThreadArgs) {
    let ThreadArgs {
        route,
        params,
        stats,
        stop,
        failures,
        init,
    } = args;
    // The COM apartment is declared first, so it is dropped last, after every COM object below.
    let com = match ComApartment::init_mta() {
        Ok(com) => com,
        Err(err) => {
            let _ = init.send(Err(err));
            return;
        }
    };
    let _mmcss = MmcssGuard::enter();
    let _denormal = DenormalGuard::new();
    let mut session = match Session::prepare(&com, &route, params, stats, stop) {
        Ok(session) => session,
        Err(err) => {
            let _ = init.send(Err(err));
            return;
        }
    };
    let _ = init.send(Ok(()));
    let outcome = session.run();
    session.stop_clients();
    if let Err(err) = outcome {
        // The controller decides whether to restart; a failed send only means it is shutting down.
        let _ = failures.send(DeviceEvent::PipelineFailed(err.to_string()));
    }
}

/// Converts the engine statistics into the `eq-core` statistics.
fn to_stats(stats: EngineStats) -> Stats {
    Stats {
        latency_ms: stats.latency_ms,
        underruns: stats.underruns,
        resyncs: stats.resyncs,
    }
}

impl From<WasapiError> for PipelineError {
    /// Reports a backend failure as a generic pipeline failure carrying the full message.
    fn from(err: WasapiError) -> Self {
        Self::Failed(err.to_string())
    }
}

/// Builds [`WasapiPipeline`]s. It holds only a channel sender, so it may live on any thread.
#[derive(Debug, Clone)]
pub struct WasapiPipelineFactory {
    failures: Sender<DeviceEvent>,
}

impl WasapiPipelineFactory {
    /// Creates a factory that reports failures of running pipelines on `failures`.
    #[must_use]
    pub fn new(failures: Sender<DeviceEvent>) -> Self {
        Self { failures }
    }
}

impl PipelineFactory for WasapiPipelineFactory {
    /// Starts the `audio-rt` thread and waits until its streams are running.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError::Failed`] when the thread cannot start, a device is missing or
    /// not float32, or a stream cannot be initialised or started.
    fn start(&self, config: PipelineConfig) -> Result<Box<dyn Pipeline>, PipelineError> {
        let PipelineConfig {
            virtual_id,
            target_id,
            latency,
            eq,
        } = config;
        let stop = OwnedEvent::new(true)?;
        let (writer, reader) = params_channel(eq);
        let stats = Arc::new(SharedStats::new());
        let (init_tx, init_rx) = bounded(1);
        let args = ThreadArgs {
            route: Route {
                virtual_id,
                target_id,
                latency,
            },
            params: reader,
            stats: Arc::clone(&stats),
            stop: stop.non_owning(),
            failures: self.failures.clone(),
            init: init_tx,
        };
        let thread = thread::Builder::new()
            .name("audio-rt".to_owned())
            .spawn(move || audio_thread(args))
            .map_err(|err| PipelineError::Failed(format!("failed to start audio thread: {err}")))?;

        match init_rx.recv() {
            Ok(Ok(())) => Ok(Box::new(WasapiPipeline {
                writer,
                stats,
                stop_event: stop,
                thread: Some(thread),
            })),
            Ok(Err(err)) => {
                let _ = thread.join();
                Err(PipelineError::from(err))
            }
            Err(_) => {
                let _ = thread.join();
                Err(PipelineError::Failed(
                    "the audio thread stopped during startup".to_owned(),
                ))
            }
        }
    }
}

/// A running WASAPI pipeline, owned by the controller. Created by
/// [`WasapiPipelineFactory::start`].
///
/// Dropping it stops the audio thread and waits for it, so the streams are released before the
/// call returns.
pub struct WasapiPipeline {
    writer: ParamsWriter,
    stats: Arc<SharedStats>,
    stop_event: OwnedEvent,
    thread: Option<JoinHandle<()>>,
}

impl WasapiPipeline {
    /// Signals the audio thread and joins it. A second call does nothing.
    fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        // The result is ignored: the stop event is a valid handle owned by `self`, so SetEvent
        // cannot fail here, and not joining would leave the thread running.
        let _ = self.stop_event.set();
        // A panicked audio thread has nothing left to clean up, so the join error is ignored.
        let _ = thread.join();
    }
}

impl Pipeline for WasapiPipeline {
    fn set_eq(&mut self, eq: EqParams) {
        self.writer.publish(eq);
    }

    fn stats(&self) -> Stats {
        to_stats(self.stats.snapshot())
    }

    fn stop(mut self: Box<Self>) {
        self.shutdown();
    }
}

impl Drop for WasapiPipeline {
    fn drop(&mut self) {
        self.shutdown();
    }
}
