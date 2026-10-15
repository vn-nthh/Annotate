//! Portable PCM level, speech detection, and recognizer WAV encoding.
pub(crate) fn rms(samples: &[i16]) -> f32 {
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

/// Keeps the fractional source position across CoreAudio callback boundaries.
// ponytail: linear interpolation; use AudioConverter if Mac speech tests show aliasing.
#[cfg(any(target_os = "macos", test))]
pub(crate) struct Resampler {
    pub samples: Vec<i16>,
    step: f64,
    next: f64,
    frame: u64,
    previous: f32,
}
#[cfg(any(target_os = "macos", test))]
impl Resampler {
    pub fn new(rate: u32) -> Self {
        assert!(rate > 0);
        Self {
            samples: Vec::new(),
            step: rate as f64 / 16000.,
            next: 0.,
            frame: 0,
            previous: 0.,
        }
    }
    pub fn push(&mut self, sample: f32) {
        let sample = if sample.is_finite() {
            sample.clamp(-1., 1.)
        } else {
            0.
        };
        if self.frame == 0 {
            self.previous = sample;
        }
        while self.next <= self.frame as f64 {
            let fraction = (self.next - (self.frame as f64 - 1.)).clamp(0., 1.) as f32;
            let value = self.previous + (sample - self.previous) * fraction;
            self.samples
                .push((value * 32768.).round().clamp(-32768., 32767.) as i16);
            self.next += self.step;
        }
        self.previous = sample;
        self.frame += 1;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn resampling_preserves_duration_and_clamps_samples() {
        let source: Vec<_> = (0..48000).map(|i| (i as f32 / 120.).sin()).collect();
        let mut whole = super::Resampler::new(48000);
        for sample in &source {
            whole.push(*sample);
        }
        assert_eq!(whole.samples.len(), 16000);
        assert!((super::rms(&whole.samples) - 0.707).abs() < 0.01);
        let mut exact = super::Resampler::new(16000);
        for sample in [2., -2., f32::NAN, 0.5] {
            exact.push(sample);
        }
        assert_eq!(exact.samples, [32767, -32768, 0, 16384]);
    }
    #[test]
    fn pcm_preserves_speech_detection_and_recognizer_format() {
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
