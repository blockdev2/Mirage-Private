//! WASAPI process loopback: all system audio except one process tree.
//!
//! Needs a Windows build that has the process-loopback virtual device; when
//! activation fails the error says so and the caller must not substitute plain
//! system loopback (that would capture the call itself).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use windows::core::{implement, Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    PROCESS_LOOPBACK_MODE, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, BLOB, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_BLOB;

use crate::system_audio::system_audio;

const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
/// `WAVE_FORMAT_IEEE_FLOAT`.
const FLOAT_FORMAT: u16 = 3;
/// How long activation may take before it is called unsupported.
const ACTIVATE_WAIT: Duration = Duration::from_secs(5);

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Completed(mpsc::Sender<()>);

impl IActivateAudioInterfaceCompletionHandler_Impl for Completed_Impl {
    fn ActivateCompleted(
        &self,
        _operation: windows::core::Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

/// Captures system audio except `exclude_pid`'s process tree into
/// [`system_audio`] until dropped.
pub struct Loopback {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Loopback {
    /// Starts the capture; fails (with the reason) when this Windows cannot.
    pub fn start(exclude_pid: u32) -> Result<Self> {
        Self::start_mode(
            exclude_pid,
            PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
        )
    }

    /// Captures only `pid`'s process tree (used by tests to tell the modes apart).
    pub fn start_only(pid: u32) -> Result<Self> {
        Self::start_mode(pid, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE)
    }

    fn start_mode(pid: u32, mode: PROCESS_LOOPBACK_MODE) -> Result<Self> {
        let exclude_pid = pid;
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("system-audio".into())
            .spawn(move || {
                // SAFETY: balanced by the CoUninitialize below on this thread.
                let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
                let started = open(exclude_pid, mode);
                match started {
                    Ok((client, capture, event)) => {
                        let _ = ready_tx.send(Ok(()));
                        pump(&client, &capture, event, &flag);
                        // SAFETY: the client was started below; the handle is ours.
                        unsafe {
                            let _ = client.Stop();
                            let _ = CloseHandle(event);
                        }
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!("{error:#}")));
                    }
                }
                if com.is_ok() {
                    // SAFETY: pairs with the successful CoInitializeEx.
                    unsafe { CoUninitialize() };
                }
            })
            .context("starting the system audio thread")?;
        match ready_rx.recv_timeout(ACTIVATE_WAIT + Duration::from_secs(2)) {
            Ok(Ok(())) => {
                system_audio().set_on(true);
                Ok(Self {
                    stop,
                    thread: Some(thread),
                })
            }
            Ok(Err(reason)) => {
                let _ = thread.join();
                Err(anyhow!(reason))
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                Err(anyhow!("system audio did not start in time"))
            }
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        system_audio().set_on(false);
    }
}

/// Activates the loopback device, initializes it for 48 kHz stereo float and
/// starts it. Returns the client, its capture service and the buffer event.
fn open(
    exclude_pid: u32,
    mode: PROCESS_LOOPBACK_MODE,
) -> Result<(IAudioClient, IAudioCaptureClient, HANDLE)> {
    let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: exclude_pid,
                ProcessLoopbackMode: mode,
            },
        },
    };
    let mut variant = PROPVARIANT::default();
    // SAFETY: a VT_BLOB variant pointing at `params`, which outlives the call;
    // it is wrapped so dropping it never tries to free the stack blob.
    let variant = unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob = BLOB {
            cbSize: std::mem::size_of_val(&params) as u32,
            pBlobData: std::ptr::addr_of_mut!(params).cast(),
        };
        std::mem::ManuallyDrop::new(variant)
    };

    let (done_tx, done_rx) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = Completed(done_tx).into();
    // SAFETY: valid path constant, IID and variant; the handler lives through the call.
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*variant),
            &handler,
        )
    }
    .context("process loopback is not available on this Windows")?;
    done_rx
        .recv_timeout(ACTIVATE_WAIT)
        .map_err(|_| anyhow!("process loopback did not answer"))?;
    let mut result = windows::core::HRESULT(0);
    let mut unknown: Option<windows::core::IUnknown> = None;
    // SAFETY: out parameters are valid locals.
    unsafe { operation.GetActivateResult(&mut result, &mut unknown) }
        .context("reading the loopback activation result")?;
    result
        .ok()
        .context("process loopback is not supported on this Windows")?;
    let client: IAudioClient = unknown
        .ok_or_else(|| anyhow!("loopback activation returned nothing"))?
        .cast()
        .context("the loopback device is not an audio client")?;

    let format = WAVEFORMATEX {
        wFormatTag: FLOAT_FORMAT,
        nChannels: CHANNELS,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * u32::from(CHANNELS) * 4,
        nBlockAlign: CHANNELS * 4,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    // SAFETY: `format` is a valid WAVEFORMATEX for the call.
    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                // 200 ms in 100 ns units.
                2_000_000,
                0,
                &format,
                None,
            )
            .context("initializing the loopback client")?;
    }
    // SAFETY: plain event creation; closed by the caller or on error below.
    let event = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
        .context("creating the capture event")?;
    // SAFETY: the client is initialized and the event handle is valid.
    let capture: IAudioCaptureClient = unsafe {
        client
            .SetEventHandle(event)
            .context("setting the capture event")?;
        let capture = client.GetService().context("getting the capture service")?;
        client.Start().context("starting the loopback client")?;
        capture
    };
    Ok((client, capture, event))
}

/// Moves packets into the shared queue as mono until `stop` is set.
fn pump(client: &IAudioClient, capture: &IAudioCaptureClient, event: HANDLE, stop: &AtomicBool) {
    let _ = client;
    let sink = system_audio();
    let mut mono: Vec<f32> = Vec::with_capacity(2_048);
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: waiting on our own event handle.
        let waited = unsafe { WaitForSingleObject(event, 100) };
        if waited != WAIT_OBJECT_0 {
            continue;
        }
        loop {
            // SAFETY: the buffer pointer is read for `frames` frames of the
            // requested stereo f32 format and released right after.
            let more = unsafe {
                let mut data = std::ptr::null_mut::<u8>();
                let mut frames = 0u32;
                let mut flags = 0u32;
                if capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .is_err()
                    || frames == 0
                {
                    break;
                }
                mono.clear();
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    mono.resize(frames as usize, 0.0);
                } else {
                    let samples = std::slice::from_raw_parts(
                        data.cast::<f32>(),
                        frames as usize * usize::from(CHANNELS),
                    );
                    mono.extend(
                        samples
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|pair| (pair[0] + pair[1]) * 0.5),
                    );
                }
                let _ = capture.ReleaseBuffer(frames);
                true
            };
            if more {
                sink.push(&mono);
            }
        }
    }
}
