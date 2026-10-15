//! Event-driven WASAPI capture; no microphone or audio thread at idle.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use windows::{
    core::HSTRING,
    Win32::{
        Foundation::*,
        Media::Audio::*,
        System::{Com::*, Threading::*},
    },
};

struct Event(HANDLE);
// A kernel event can be signaled/waited from any thread. The owning Arc keeps
// its handle alive until both the capture worker and caller have finished.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}
impl Event {
    fn new(manual_reset: bool) -> windows::core::Result<Self> {
        unsafe { CreateEventW(None, manual_reset, false, None).map(Self) }
    }
}
impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
struct Stop {
    requested: AtomicBool,
    event: windows::core::Result<Event>,
}
impl Stop {
    fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
            event: Event::new(true),
        }
    }
    fn signal(&self) {
        self.requested.store(true, Ordering::Release);
        if let Ok(event) = &self.event {
            unsafe {
                let _ = SetEvent(event.0);
            }
        }
    }
}

pub struct Capture {
    stop: Arc<Stop>,
    worker: Option<std::thread::JoinHandle<Result<Vec<i16>, String>>>,
}
impl Capture {
    pub fn start_with_error(
        device_id: String,
        on_error: impl FnOnce(String) + Send + 'static,
    ) -> Self {
        let stop = Arc::new(Stop::new());
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            let result = unsafe { record(&device_id, &worker_stop).map_err(|e| e.to_string()) };
            if let Err(error) = &result {
                on_error(error.clone());
            }
            result
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
    pub fn stop(mut self) -> Result<Vec<i16>, String> {
        self.stop.signal();
        self.worker
            .take()
            .unwrap()
            .join()
            .map_err(|_| "Microphone thread failed".to_string())?
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.signal();
    }
}

unsafe fn record(id: &str, stop: &Stop) -> windows::core::Result<Vec<i16>> {
    // A quick key release can arrive before the audio thread gets scheduled.
    // In that case there is no reason to initialize COM or open a microphone.
    if stop.requested.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    let stop_event = stop.event.as_ref().map_err(Clone::clone)?;
    CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    let _com = Com;
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let device = if id.is_empty() {
        enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)?
    } else {
        enumerator.GetDevice(&HSTRING::from(id))?
    };
    // Keep the callback handle alive until IAudioClient releases its stream.
    let audio_event = Event::new(false)?;
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
    let format = WAVEFORMATEX {
        wFormatTag: 1,
        nChannels: 1,
        nSamplesPerSec: 16000,
        nAvgBytesPerSec: 32000,
        nBlockAlign: 2,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    // The shared audio engine performs the rate/channel conversion for every
    // provider, preserving the app's previous 16 kHz mono PCM16 WAV format.
    client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
            | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        0,
        0,
        &format,
        None,
    )?;
    client.SetEventHandle(audio_event.0)?;
    let capture: IAudioCaptureClient = client.GetService()?;
    if stop.requested.load(Ordering::Acquire) {
        return Ok(Vec::new());
    }
    client.Start()?;
    let mut samples = Vec::new();
    let result = (|| {
        while !stop.requested.load(Ordering::Acquire) {
            let ready = WaitForMultipleObjects(&[stop_event.0, audio_event.0], false, 1000);
            if ready == WAIT_FAILED {
                return Err(windows::core::Error::from_win32());
            }
            drain(&capture, &mut samples)?;
        }
        Ok(())
    })();
    let stopped = client.Stop();
    result?;
    stopped?;
    // Stop preserves queued packets. Drain once more so the last syllable is
    // retained even when key-up arrives between audio-engine callbacks.
    drain(&capture, &mut samples)?;
    Ok(samples)
}

unsafe fn drain(
    capture: &IAudioCaptureClient,
    samples: &mut Vec<i16>,
) -> windows::core::Result<()> {
    while capture.GetNextPacketSize()? > 0 {
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames = 0;
        let mut flags = 0;
        capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
        if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || frames == 0 {
            samples.resize(samples.len() + frames as usize, 0);
            let _ = crate::ui::overlay::set_level(0.0);
        } else {
            let packet = std::slice::from_raw_parts(data.cast::<i16>(), frames as usize);
            let _ = crate::ui::overlay::set_level((rms(packet) * 5.0).min(1.0));
            samples.extend_from_slice(packet);
        }
        capture.ReleaseBuffer(frames)?;
        if samples.len() >= 16000 * 60 * 10 {
            return Err(windows::core::Error::from_hresult(E_ABORT));
        }
    }
    Ok(())
}
fn rms(samples: &[i16]) -> f32 {
    (samples
        .iter()
        .map(|n| (*n as f32 / 32768.0).powi(2))
        .sum::<f32>()
        / samples.len().max(1) as f32)
        .sqrt()
}
pub fn speech_present(samples: &[i16]) -> bool {
    samples.chunks(512).any(|chunk| rms(chunk) >= 0.01)
}
pub fn wav(samples: &[i16]) -> Result<Vec<u8>, String> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(
            &mut cursor,
            hound::WavSpec {
                channels: 1,
                sample_rate: 16000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .map_err(|e| e.to_string())?;
        for sample in samples {
            writer.write_sample(*sample).map_err(|e| e.to_string())?;
        }
        writer.finalize().map_err(|e| e.to_string())?;
    }
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    #[test]
    fn utterance_capture_preserves_cancellation_and_recognizer_format() {
        let stop = super::Stop::new();
        stop.signal();
        assert!(
            unsafe { super::record("this endpoint must never be opened", &stop) }
                .unwrap()
                .is_empty()
        );
        let event = stop.event.as_ref().unwrap();
        assert_eq!(
            unsafe { super::WaitForSingleObject(event.0, 0) },
            super::WAIT_OBJECT_0
        );
        assert!(!super::speech_present(&vec![0; 16000]));
        let voice = vec![4000; 16000];
        assert!(super::speech_present(&voice));
        let bytes = super::wav(&voice).unwrap();
        let reader = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(reader.spec().sample_rate, 16000);
        assert_eq!(reader.spec().channels, 1);
        assert_eq!(
            reader
                .into_samples::<i16>()
                .map(Result::unwrap)
                .collect::<Vec<_>>(),
            voice
        );
    }
}
