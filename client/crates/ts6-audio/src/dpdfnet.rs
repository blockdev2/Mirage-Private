//! Streaming speech enhancement with DPDFNet (Ceva, Apache-2.0).
//!
//! DPDFNet is a causal neural denoiser (DeepFilterNet2 with dual-path RNN
//! blocks). This module runs its 48 kHz ONNX model with `tract`, a pure-Rust
//! ONNX runtime, following the reference streaming loop of the upstream
//! `dpdfnet.StreamEnhancer`:
//!
//! * a 960-sample (20 ms) Vorbis-windowed frame every 480 samples (10 ms),
//! * a real FFT of each frame, fed to the model together with its recurrent
//!   state, which comes back updated,
//! * an inverse FFT of the enhanced spectrum, windowed again and overlap-added.
//!
//! The model's input is exactly our 48 kHz mono, so there is no resampling. The
//! output trails the input by one hop (10 ms). The model file is embedded in the
//! binary; its license is in `models/LICENSE-DPDFNet`.

use std::io::Cursor;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use tract_onnx::prelude::*;

/// The 48 kHz DPDFNet-2 model.
static MODEL: &[u8] = include_bytes!("../models/dpdfnet2_48khz_hr.onnx");

type Plan = Arc<TypedRunnableModel>;

pub struct DpdfNet {
    plan: Plan,
    state: Vec<f32>,
    initial_state: Vec<f32>,
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    window: Vec<f32>,
    /// The last `window` input samples (older hop first).
    input: Vec<f32>,
    /// Overlap-add accumulator for the output.
    output: Vec<f32>,
    frame: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    hop: usize,
    freq_bins: usize,
}

/// The model facts DPDFNet writes into the ONNX file's metadata.
struct Meta {
    window: usize,
    hop: usize,
    freq_bins: usize,
    initial_state: Vec<f32>,
}

fn number<T: std::str::FromStr>(meta: &[(String, String)], key: &str) -> Result<T> {
    meta.iter()
        .find(|(name, _)| name == key)
        .ok_or_else(|| anyhow!("DPDFNet model has no `{key}` metadata"))?
        .1
        .trim()
        .parse()
        .map_err(|_| anyhow!("DPDFNet metadata `{key}` is not a number"))
}

fn floats(meta: &[(String, String)], key: &str) -> Result<Vec<f32>> {
    meta.iter()
        .find(|(name, _)| name == key)
        .ok_or_else(|| anyhow!("DPDFNet model has no `{key}` metadata"))?
        .1
        .split(',')
        .map(|value| {
            value
                .trim()
                .parse()
                .map_err(|_| anyhow!("DPDFNet metadata `{key}` holds a non-number"))
        })
        .collect()
}

impl Meta {
    fn read(meta: &[(String, String)]) -> Result<Self> {
        let sample_rate: u32 = number(meta, "sample_rate")?;
        if sample_rate != 48_000 {
            bail!("DPDFNet model runs at {sample_rate} Hz, expected 48000");
        }
        let window: usize = number(meta, "window_length")?;
        let hop: usize = number(meta, "hop_length")?;
        let freq_bins: usize = number(meta, "freq_bins")?;
        if window != 2 * hop || freq_bins != window / 2 + 1 {
            bail!("DPDFNet model has an unexpected frame layout");
        }
        // The recurrent state starts as zeros except for two normalizer
        // sections that the model file provides.
        let size: usize = number(meta, "state_size")?;
        let erb_len: usize = number(meta, "erb_norm_state_size")?;
        let spec_len: usize = number(meta, "spec_norm_state_size")?;
        let erb = floats(meta, "erb_norm_init")?;
        let spec = floats(meta, "spec_norm_init")?;
        if erb.len() != erb_len || spec.len() != spec_len || erb_len + spec_len > size {
            bail!("DPDFNet state metadata is inconsistent");
        }
        let mut initial_state = vec![0.0; size];
        initial_state[..erb_len].copy_from_slice(&erb);
        initial_state[erb_len..erb_len + spec_len].copy_from_slice(&spec);
        Ok(Self {
            window,
            hop,
            freq_bins,
            initial_state,
        })
    }
}

/// Vorbis window, the one DPDFNet was trained with. It satisfies
/// `w[n]^2 + w[n + hop]^2 == 1`, so the overlap-add reconstructs exactly.
fn vorbis_window(len: usize) -> Vec<f32> {
    let half = len as f32 / 2.0;
    (0..len)
        .map(|n| {
            let s = (0.5 * std::f32::consts::PI * (n as f32 + 0.5) / half).sin();
            (0.5 * std::f32::consts::PI * s * s).sin()
        })
        .collect()
}

impl DpdfNet {
    /// Loads the embedded model. Takes a fraction of a second, so do it away
    /// from a real-time thread.
    pub fn new() -> Result<Self> {
        let onnx = tract_onnx::onnx();
        let proto = onnx
            .proto_model_for_read(&mut Cursor::new(MODEL))
            .context("reading the DPDFNet model")?;
        let metadata: Vec<(String, String)> = proto
            .metadata_props
            .iter()
            .map(|prop| (prop.key.clone(), prop.value.clone()))
            .collect();
        let meta = Meta::read(&metadata)?;
        let plan = onnx
            .model_for_proto_model(&proto)
            .and_then(|model| model.into_optimized())
            .and_then(|model| model.into_runnable())
            .context("preparing the DPDFNet model")?;

        let mut planner = RealFftPlanner::<f32>::new();
        Ok(Self {
            plan,
            state: meta.initial_state.clone(),
            initial_state: meta.initial_state,
            forward: planner.plan_fft_forward(meta.window),
            inverse: planner.plan_fft_inverse(meta.window),
            window: vorbis_window(meta.window),
            input: vec![0.0; meta.window],
            output: vec![0.0; meta.window],
            frame: vec![0.0; meta.window],
            spectrum: vec![Complex::new(0.0, 0.0); meta.freq_bins],
            hop: meta.hop,
            freq_bins: meta.freq_bins,
        })
    }

    /// Samples per step: 10 ms at 48 kHz.
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// Forgets the recurrent state and the audio in flight, as for a new stream.
    pub fn reset(&mut self) {
        self.state.copy_from_slice(&self.initial_state);
        self.input.fill(0.0);
        self.output.fill(0.0);
    }

    /// Enhances `samples` in place. Whole hops only: a shorter tail is left as
    /// it is. If the model fails on a hop, that hop passes through unchanged.
    pub fn process(&mut self, samples: &mut [f32]) {
        for hop in samples.chunks_exact_mut(self.hop) {
            if let Err(error) = self.process_hop(hop) {
                tracing::warn!(%error, "DPDFNet failed on a frame; passing audio through");
            }
        }
    }

    fn process_hop(&mut self, hop: &mut [f32]) -> Result<()> {
        let window = self.window.len();
        // Slide the analysis window by one hop and take the new samples in.
        self.input.copy_within(self.hop.., 0);
        self.input[window - self.hop..].copy_from_slice(hop);

        for ((slot, sample), weight) in self.frame.iter_mut().zip(&self.input).zip(&self.window) {
            *slot = sample * weight;
        }
        self.forward
            .process(&mut self.frame, &mut self.spectrum)
            .map_err(|error| anyhow!("forward FFT: {error}"))?;

        let mut spec = Vec::with_capacity(self.freq_bins * 2);
        for bin in &self.spectrum {
            spec.push(bin.re);
            spec.push(bin.im);
        }
        let spec = tract_ndarray::Array4::from_shape_vec((1, 1, self.freq_bins, 2), spec)?;
        let state = tract_ndarray::Array1::from_vec(std::mem::take(&mut self.state));
        let result = self
            .plan
            .run(tvec!(spec.into_tvalue(), state.into_tvalue()));
        let outputs = match result {
            Ok(outputs) => outputs,
            Err(error) => {
                // The state was moved into the failed call: start it over.
                self.state.clone_from(&self.initial_state);
                return Err(anyhow!("model run: {error}"));
            }
        };
        let enhanced = outputs[0].to_plain_array_view::<f32>()?;
        self.state = outputs[1]
            .to_plain_array_view::<f32>()?
            .iter()
            .copied()
            .collect();

        let (pairs, _) = enhanced.as_slice().unwrap_or(&[]).as_chunks::<2>();
        for (bin, [re, im]) in self.spectrum.iter_mut().zip(pairs) {
            *bin = Complex::new(*re, *im);
        }
        // The real inverse transform ignores these two imaginary parts; zeroing
        // them keeps it from complaining about values it cannot represent.
        self.spectrum[0].im = 0.0;
        if let Some(last) = self.spectrum.last_mut() {
            last.im = 0.0;
        }
        self.inverse
            .process(&mut self.spectrum, &mut self.frame)
            .map_err(|error| anyhow!("inverse FFT: {error}"))?;

        // The inverse transform is not normalized.
        let scale = 1.0 / window as f32;
        for ((acc, value), weight) in self.output.iter_mut().zip(&self.frame).zip(&self.window) {
            *acc += value * scale * weight;
        }
        // The first hop is complete now: hand it back and slide the rest down.
        for (slot, value) in hop.iter_mut().zip(&self.output) {
            *slot = value.clamp(-1.0, 1.0);
        }
        self.output.copy_within(self.hop.., 0);
        let tail = window - self.hop;
        self.output[tail..].fill(0.0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vad::level_db;

    /// Deterministic white-ish noise.
    fn noise(len: usize, amplitude: f32, seed: &mut u32) -> Vec<f32> {
        (0..len)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0) * amplitude
            })
            .collect()
    }

    #[test]
    fn the_vorbis_window_reconstructs_at_half_overlap() {
        let window = vorbis_window(960);
        for n in 0..480 {
            let sum = window[n].powi(2) + window[n + 480].powi(2);
            assert!((sum - 1.0).abs() < 1e-5, "n={n} sum={sum}");
        }
    }

    #[test]
    fn the_embedded_model_loads_and_describes_itself() {
        let model = DpdfNet::new().expect("model loads");
        assert_eq!(model.hop(), 480);
        assert_eq!(model.state.len(), 56_436);
    }

    #[test]
    fn silence_stays_silent() {
        let mut model = DpdfNet::new().unwrap();
        let mut samples = vec![0.0; 960 * 10];
        model.process(&mut samples);
        assert!(
            samples.iter().all(|s| s.abs() < 1e-3),
            "silence became audible"
        );
    }

    #[test]
    fn steady_noise_is_attenuated() {
        let mut model = DpdfNet::new().unwrap();
        let mut seed = 7;
        let (mut before, mut after) = (0.0, 0.0);
        for index in 0..100 {
            let mut frame = noise(960, 0.05, &mut seed);
            let input = level_db(&frame);
            model.process(&mut frame);
            if index >= 50 {
                before += input;
                after += level_db(&frame);
            }
        }
        let reduction = (before - after) / 50.0;
        assert!(reduction > 6.0, "only {reduction:.1} dB of noise removed");
    }

    #[test]
    fn it_keeps_up_with_real_time() {
        let mut model = DpdfNet::new().unwrap();
        let mut frame = vec![0.01f32; 960];
        let start = std::time::Instant::now();
        for _ in 0..200 {
            model.process(&mut frame);
        }
        // 200 frames are 4 s of audio; slower than that cannot run live.
        // Shared CI runners are slow, so no extra margin is demanded here.
        let took = start.elapsed();
        assert!(
            took < std::time::Duration::from_secs(4),
            "too slow: {took:?}"
        );
    }

    #[test]
    fn reset_restores_the_starting_point() {
        let mut model = DpdfNet::new().unwrap();
        let mut seed = 5;
        let signal = noise(960 * 5, 0.05, &mut seed);
        let mut first = signal.clone();
        model.process(&mut first);
        model.reset();
        let mut second = signal;
        model.process(&mut second);
        let worst = first
            .iter()
            .zip(&second)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(worst < 1e-6, "output differs after reset by {worst}");
    }
}
