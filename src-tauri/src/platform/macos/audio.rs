//! CoreAudio streams exist only while recording. CPAL owns the native stream;
//! conversion and level reporting reuse the portable PCM helpers.
use crate::{
    native_audio::{rms, Resampler},
    AudioDevice,
};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    FromSample, Sample, SizedSample,
};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Default)]
struct Status {
    stop: bool,
    error: Option<String>,
    permission: Option<bool>,
}
#[derive(Default)]
struct Stop {
    status: Mutex<Status>,
    changed: Condvar,
}
impl Stop {
    fn signal(&self) {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
        self.changed.notify_all();
    }
}
pub struct Capture {
    stop: Arc<Stop>,
    worker: Option<std::thread::JoinHandle<Result<Vec<i16>, String>>>,
}
impl Capture {
    pub fn start_with_error(
        device: String,
        on_error: impl FnOnce(String) + Send + 'static,
    ) -> Self {
        let stop = Arc::new(Stop::default());
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || {
            let result = record(&device, worker_stop);
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
            .map_err(|_| "The microphone thread failed".to_string())?
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.signal();
    }
}

fn permission(stop: &Arc<Stop>) -> Result<bool, String> {
    use cocoa::base::{id, BOOL, YES};
    use objc::{class, msg_send, sel, sel_impl};
    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {
        static AVMediaTypeAudio: id;
    }
    objc::rc::autoreleasepool(|| unsafe {
        let status: isize =
            msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: AVMediaTypeAudio];
        if status == 3 {
            return Ok(true);
        }
        if status != 0 {
            return Err("Allow Annotate in System Settings > Privacy & Security > Microphone, then try again.".into());
        }
        let notification = stop.clone();
        let callback = block::ConcreteBlock::new(move |granted: BOOL| {
            notification
                .status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .permission = Some(granted == YES);
            notification.changed.notify_all();
        })
        .copy();
        let _: () = msg_send![class!(AVCaptureDevice), requestAccessForMediaType: AVMediaTypeAudio completionHandler: &*callback];
        let status = stop
            .changed
            .wait_while(stop.status.lock().unwrap_or_else(|e| e.into_inner()), |s| {
                !s.stop && s.permission.is_none()
            })
            .unwrap_or_else(|e| e.into_inner());
        if status.stop {
            Ok(false)
        } else if status.permission == Some(true) {
            Ok(true)
        } else {
            Err("Microphone access was denied. Allow Annotate in System Settings > Privacy & Security > Microphone.".into())
        }
    })
}
fn record(device_id: &str, stop: Arc<Stop>) -> Result<Vec<i16>, String> {
    if stop.status.lock().unwrap_or_else(|e| e.into_inner()).stop || !permission(&stop)? {
        return Ok(Vec::new());
    }
    let host = cpal::default_host();
    let device = if device_id.is_empty() {
        host.default_input_device()
    } else {
        let id = device_id
            .parse::<cpal::DeviceId>()
            .map_err(|e| e.to_string())?;
        host.device_by_id(&id)
    }
    .ok_or("The selected microphone is unavailable. Choose another microphone in Settings.")?;
    let config = device.default_input_config().map_err(|e| e.to_string())?;
    if config.channels() == 0 || config.sample_rate() == 0 {
        return Err("The microphone returned an invalid audio format".into());
    }
    let pcm = Arc::new(Mutex::new(Resampler::new(config.sample_rate())));
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            stream::<f32>(&device, config.config(), stop.clone(), pcm.clone())
        }
        cpal::SampleFormat::F64 => {
            stream::<f64>(&device, config.config(), stop.clone(), pcm.clone())
        }
        cpal::SampleFormat::I16 => {
            stream::<i16>(&device, config.config(), stop.clone(), pcm.clone())
        }
        cpal::SampleFormat::I32 => {
            stream::<i32>(&device, config.config(), stop.clone(), pcm.clone())
        }
        _ => {
            return Err(
                "The microphone's sample format is unsupported. Choose another microphone.".into(),
            )
        }
    }?;
    if stop.status.lock().unwrap_or_else(|e| e.into_inner()).stop {
        return Ok(Vec::new());
    }
    stream.play().map_err(|e| e.to_string())?;
    let status = stop
        .changed
        .wait_while(stop.status.lock().unwrap_or_else(|e| e.into_inner()), |s| {
            !s.stop
        })
        .unwrap_or_else(|e| e.into_inner());
    let error = status.error.clone();
    drop(status);
    drop(stream);
    if let Some(error) = error {
        return Err(error);
    }
    let samples = std::mem::take(&mut pcm.lock().unwrap_or_else(|e| e.into_inner()).samples);
    Ok(samples)
}
fn stream<T: SizedSample + Sample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    stop: Arc<Stop>,
    pcm: Arc<Mutex<Resampler>>,
) -> Result<cpal::Stream, String>
where
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    if channels == 0 || config.sample_rate == 0 {
        return Err("The microphone returned an invalid audio format".into());
    }
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data, _| {
                let mut pcm = pcm.lock().unwrap_or_else(|e| e.into_inner());
                let start = pcm.samples.len();
                for frame in data.chunks_exact(channels) {
                    let mono = frame
                        .iter()
                        .map(|value| (*value).to_sample::<f32>())
                        .sum::<f32>()
                        / channels as f32;
                    pcm.push(mono);
                }
                let _ = crate::ui::overlay::set_level(rms(&pcm.samples[start..]));
            },
            move |error| {
                let mut status = stop.status.lock().unwrap_or_else(|e| e.into_inner());
                status.error = Some(format!("Microphone capture failed: {error}"));
                status.stop = true;
                stop.changed.notify_all();
            },
            Some(std::time::Duration::from_secs(5)),
        )
        .map_err(|e| e.to_string())
}
pub fn enumerate_capture_devices() -> Result<Vec<AudioDevice>, Box<dyn std::error::Error>> {
    cpal::default_host()
        .input_devices()?
        .map(|device| {
            Ok(AudioDevice {
                id: device.id()?.to_string(),
                name: device.to_string(),
            })
        })
        .collect()
}
