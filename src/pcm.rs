pub const SAMPLE_RATE: u32 = 24_000;

pub fn resample_linear(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if input.is_empty() || from_rate == 0 || to_rate == 0 || from_rate == to_rate {
        return input.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = ((input.len() as f64) / ratio).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(out_len);
    let last = input.len() - 1;
    for i in 0..out_len {
        let src = i as f64 * ratio;
        let idx = (src.floor() as usize).min(last);
        let next = (idx + 1).min(last);
        let frac = (src - idx as f64) as f32;
        let sample = input[idx] + (input[next] - input[idx]) * frac;
        out.push(sample.clamp(-1.0, 1.0));
    }
    out
}

pub fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|sample| *sample as f32 / 32768.0).collect()
}

pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| (sample.clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect()
}

pub fn i16_to_le(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

pub fn i16_from_le(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect()
}

pub fn rms_f32(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy: f32 = samples.iter().map(|sample| sample * sample).sum();
    (energy / samples.len() as f32).sqrt()
}

pub fn rms_i16(samples: &[i16]) -> f32 {
    rms_f32(&i16_to_f32(samples))
}

pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Subtract a delayed copy of playback from the microphone.
///
/// `reference` is chronological playback, index 0 oldest. `delay_samples` is how
/// many samples the speaker is behind the newest reference sample.
pub fn echo_subtract(mic: &mut [f32], reference: &[f32], delay_samples: usize, gain: f32) {
    if gain == 0.0 || mic.is_empty() || reference.is_empty() {
        return;
    }
    let start = reference.len() as isize - delay_samples as isize - mic.len() as isize;
    for (offset, sample) in mic.iter_mut().enumerate() {
        let index = start + offset as isize;
        if index >= 0 {
            if let Some(played) = reference.get(index as usize) {
                *sample = (*sample - gain * played).clamp(-1.0, 1.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_resample_keeps_samples() {
        let input = vec![0.1, -0.2, 0.3];
        assert_eq!(resample_linear(&input, 24_000, 24_000), input);
    }

    #[test]
    fn upsample_grows_the_buffer() {
        let input = vec![0.0, 1.0, 0.0, -1.0];
        let output = resample_linear(&input, 16_000, 32_000);
        assert_eq!(output.len(), 8);
    }

    #[test]
    fn echo_subtract_removes_a_delayed_copy() {
        let reference = vec![0.0, 0.0, 0.5, -0.25, 0.75];
        let mut mic = vec![0.5, -0.25];
        echo_subtract(&mut mic, &reference, 1, 1.0);
        assert!(mic.iter().all(|sample| sample.abs() < 1e-5));
    }

    #[test]
    fn pcm16_roundtrip() {
        let samples = vec![0, 12, -400, 32767, -32768];
        assert_eq!(i16_from_le(&i16_to_le(&samples)), samples);
    }
}
