use crate::pcm::{f32_to_i16, SAMPLE_RATE};

#[derive(Clone, Copy, Debug)]
pub enum Earcon {
    Ack,
    Listen,
    Timer,
    Alert,
}

pub fn synth(kind: Earcon) -> Vec<i16> {
    let tones: &[(f32, u32)] = match kind {
        Earcon::Ack => &[(660.0, 70)],
        Earcon::Listen => &[(880.0, 50), (1320.0, 70)],
        Earcon::Timer => &[(880.0, 90), (0.0, 40), (880.0, 90), (0.0, 40), (1175.0, 140)],
        Earcon::Alert => &[(980.0, 120), (0.0, 50), (980.0, 120), (0.0, 50), (980.0, 180)],
    };
    let mut samples = Vec::new();
    for (freq, millis) in tones {
        let count = SAMPLE_RATE as usize * *millis as usize / 1000;
        for i in 0..count {
            let envelope = if *freq == 0.0 {
                0.0
            } else {
                let t = i as f32 / SAMPLE_RATE as f32;
                (2.0 * std::f32::consts::PI * freq * t).sin() * 0.22
            };
            samples.push(envelope);
        }
    }
    f32_to_i16(&samples)
}
