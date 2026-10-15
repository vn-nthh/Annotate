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

#[cfg(test)]
mod tests {
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
