//! Small DSP blocks: a look-ahead peak limiter and a 48 kHz stereo → 16 kHz mono downsampler.

use std::collections::VecDeque;

use wide::f32x8;

/// `dst += src`, eight lanes at a time.
pub fn add_into(dst: &mut [f32], src: &[f32]) {
    let n = dst.len().min(src.len());
    let split = n - n % 8;
    for (d, s) in dst[..split].chunks_exact_mut(8).zip(src[..split].chunks_exact(8)) {
        let v = f32x8::from(<[f32; 8]>::try_from(&*d).unwrap()) + f32x8::from(<[f32; 8]>::try_from(s).unwrap());
        d.copy_from_slice(&v.to_array());
    }
    for (d, s) in dst[split..n].iter_mut().zip(&src[split..n]) {
        *d += s;
    }
}

/// Keeps stereo peaks under `ceiling` without clipping. The gain drops *before* a peak
/// arrives (5 ms look-ahead) and recovers smoothly (~100 ms). Output lags input by the
/// look-ahead; `flush` emits the tail so no samples are lost.
pub struct PeakLimiter {
    ceiling: f32,
    look: usize,
    delay: VecDeque<[f32; 2]>,
    need: VecDeque<(u64, f32)>,
    idx: u64,
    gain: f32,
    release: f32,
}

impl PeakLimiter {
    pub fn new(ceiling: f32) -> Self {
        let look = 240; // 5 ms at 48 kHz
        Self {
            ceiling,
            look,
            delay: VecDeque::with_capacity(look + 1),
            need: VecDeque::new(),
            idx: 0,
            gain: 1.0,
            release: 1.0 / 4800.0,
        }
    }

    fn push(&mut self, l: f32, r: f32, out: &mut Vec<f32>) {
        let peak = l.abs().max(r.abs());
        let req = if peak > self.ceiling { self.ceiling / peak } else { 1.0 };
        while self.need.back().is_some_and(|&(_, g)| g >= req) {
            self.need.pop_back();
        }
        self.need.push_back((self.idx, req));
        while self.need.front().is_some_and(|&(i, _)| i + (self.look as u64) < self.idx) {
            self.need.pop_front();
        }
        let target = self.need.front().map_or(1.0, |&(_, g)| g);
        self.gain = if target < self.gain {
            target
        } else {
            self.gain + (target - self.gain) * self.release
        };
        self.delay.push_back([l, r]);
        if self.delay.len() > self.look {
            let [a, b] = self.delay.pop_front().unwrap();
            out.extend_from_slice(&[a * self.gain, b * self.gain]);
        }
        self.idx += 1;
    }

    pub fn process(&mut self, stereo: &[f32], out: &mut Vec<f32>) {
        for f in stereo.chunks_exact(2) {
            self.push(f[0], f[1], out);
        }
    }

    pub fn flush(&mut self, out: &mut Vec<f32>) {
        while let Some([a, b]) = self.delay.pop_front() {
            let g = self.gain.min(self.ceiling / a.abs().max(b.abs()).max(self.ceiling));
            out.extend_from_slice(&[a * g, b * g]);
        }
    }
}

/// 48 kHz stereo → 16 kHz mono for speech: average channels, low-pass (windowed sinc,
/// 7 kHz cut-off, 63 taps), keep every third sample.
pub struct Downsampler {
    taps: Vec<f32>,
    hist: VecDeque<f32>,
    phase: usize,
}

impl Default for Downsampler {
    fn default() -> Self {
        let n = 63;
        let fc = 7_000.0 / 48_000.0;
        let mid = (n / 2) as f32;
        let mut taps: Vec<f32> = (0..n)
            .map(|i| {
                let x = i as f32 - mid;
                let sinc = if x == 0.0 {
                    2.0 * fc
                } else {
                    (2.0 * std::f32::consts::PI * fc * x).sin() / (std::f32::consts::PI * x)
                };
                let w = 0.42 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos()
                    + 0.08 * (4.0 * std::f32::consts::PI * i as f32 / (n - 1) as f32).cos();
                sinc * w
            })
            .collect();
        let sum: f32 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        Self {
            hist: VecDeque::from(vec![0.0; n]),
            taps,
            phase: 0,
        }
    }
}

impl Downsampler {
    pub fn process(&mut self, stereo: &[f32], out: &mut Vec<f32>) {
        for f in stereo.chunks_exact(2) {
            self.hist.pop_front();
            self.hist.push_back((f[0] + f[1]) * 0.5);
            if self.phase == 0 {
                out.push(self.hist.iter().zip(&self.taps).map(|(h, t)| h * t).sum());
            }
            self.phase = (self.phase + 1) % 3;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_keeps_length_and_ceiling() {
        let mut lim = PeakLimiter::new(0.95);
        let input: Vec<f32> = (0..4800)
            .flat_map(|i| {
                let v = if i > 2000 { 1.5 } else { 0.5 };
                [v, -v]
            })
            .collect();
        let mut out = Vec::new();
        lim.process(&input, &mut out);
        lim.flush(&mut out);
        assert_eq!(out.len(), input.len());
        assert!(out.iter().all(|s| s.abs() <= 0.95 + 1e-6));
        assert!((out[2 * 100] - 0.5).abs() < 1e-6); // quiet part untouched
    }

    #[test]
    fn downsampler_keeps_speech_band_and_rate() {
        let tone = |hz: f32| -> Vec<f32> {
            (0..48_000)
                .flat_map(|i| {
                    let s = (2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin();
                    [s, s]
                })
                .collect()
        };
        let rms = |v: &[f32]| (v[1000..].iter().map(|x| x * x).sum::<f32>() / (v.len() - 1000) as f32).sqrt();
        let (mut a, mut b) = (Vec::new(), Vec::new());
        Downsampler::default().process(&tone(1_000.0), &mut a);
        Downsampler::default().process(&tone(15_000.0), &mut b);
        assert_eq!(a.len(), 16_000);
        assert!(rms(&a) > 0.65, "1 kHz passes: {}", rms(&a));
        assert!(rms(&b) < 0.01, "15 kHz is removed: {}", rms(&b));
    }

    #[test]
    fn simd_add_matches_scalar() {
        let mut d: Vec<f32> = (0..21).map(|i| i as f32).collect();
        add_into(&mut d, &[1.0; 21]);
        assert_eq!(d, (0..21).map(|i| i as f32 + 1.0).collect::<Vec<_>>());
    }
}
