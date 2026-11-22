//! Zero-latency, uniformly partitioned FFT convolution (overlap-add with a frequency-domain
//! delay line). All memory is allocated up front in `new()`, so `process()` is real-time safe.

use realfft::num_complex::Complex32;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Partition size. The FFT size is twice this.
pub const BLOCK: usize = 512;

pub struct Convolver {
    num_parts: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
    /// Spectra of the impulse response partitions, pre-scaled by 1/N.
    ir_parts: Vec<Vec<Complex32>>,
    /// Frequency-domain delay line: spectra of past input blocks (ring buffer).
    fdl: Vec<Vec<Complex32>>,
    fdl_pos: usize,
    /// Sum of all partitions except the first, for the block currently being filled.
    premult: Vec<Complex32>,
    in_buf: Vec<f32>,
    in_pos: usize,
    time_buf: Vec<f32>,
    spec_buf: Vec<Complex32>,
    out_buf: Vec<f32>,
    overlap: Vec<f32>,
    scratch_fwd: Vec<Complex32>,
    scratch_inv: Vec<Complex32>,
}

impl Convolver {
    pub fn new(ir: &[f32]) -> Self {
        let n = BLOCK * 2;
        let bins = BLOCK + 1;
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let ifft = planner.plan_fft_inverse(n);
        let num_parts = ir.len().div_ceil(BLOCK).max(1);

        let mut scratch_fwd = fft.make_scratch_vec();
        let scale = 1.0 / n as f32;
        let mut ir_parts = Vec::with_capacity(num_parts);
        let mut tmp = vec![0.0f32; n];
        for p in 0..num_parts {
            tmp.iter_mut().for_each(|x| *x = 0.0);
            let start = p * BLOCK;
            let end = (start + BLOCK).min(ir.len());
            for (i, &s) in ir[start..end].iter().enumerate() {
                tmp[i] = s * scale;
            }
            let mut spec = vec![Complex32::new(0.0, 0.0); bins];
            fft.process_with_scratch(&mut tmp, &mut spec, &mut scratch_fwd)
                .expect("fft size mismatch");
            ir_parts.push(spec);
        }

        Self {
            num_parts,
            scratch_inv: ifft.make_scratch_vec(),
            fft,
            ifft,
            ir_parts,
            fdl: vec![vec![Complex32::new(0.0, 0.0); bins]; num_parts],
            fdl_pos: 0,
            premult: vec![Complex32::new(0.0, 0.0); bins],
            in_buf: vec![0.0; BLOCK],
            in_pos: 0,
            time_buf: vec![0.0; n],
            spec_buf: vec![Complex32::new(0.0, 0.0); bins],
            out_buf: vec![0.0; n],
            overlap: vec![0.0; BLOCK],
            scratch_fwd,
        }
    }

    /// Clears all internal history.
    pub fn reset(&mut self) {
        for spec in self.fdl.iter_mut() {
            spec.iter_mut().for_each(|c| *c = Complex32::new(0.0, 0.0));
        }
        self.premult.iter_mut().for_each(|c| *c = Complex32::new(0.0, 0.0));
        self.in_buf.iter_mut().for_each(|x| *x = 0.0);
        self.overlap.iter_mut().for_each(|x| *x = 0.0);
        self.in_pos = 0;
        self.fdl_pos = 0;
    }

    /// Convolves `input` with the impulse response and writes the result to `output`.
    /// Zero latency: output sample `i` includes the contribution of input sample `i`.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) {
        debug_assert_eq!(input.len(), output.len());
        let mut done = 0;
        while done < input.len() {
            let n = (input.len() - done).min(BLOCK - self.in_pos);
            self.in_buf[self.in_pos..self.in_pos + n].copy_from_slice(&input[done..done + n]);

            // Transform the (partially filled, zero padded) current block
            self.time_buf[..BLOCK].copy_from_slice(&self.in_buf);
            self.time_buf[BLOCK..].iter_mut().for_each(|x| *x = 0.0);
            let cur = &mut self.fdl[self.fdl_pos];
            let _ = self
                .fft
                .process_with_scratch(&mut self.time_buf, cur, &mut self.scratch_fwd);

            // Current block times the first partition, plus everything older
            let h0 = &self.ir_parts[0];
            for ((out, (&x, &h)), &pm) in self
                .spec_buf
                .iter_mut()
                .zip(cur.iter().zip(h0.iter()))
                .zip(self.premult.iter())
            {
                *out = x * h + pm;
            }
            let last = self.spec_buf.len() - 1;
            self.spec_buf[0].im = 0.0;
            self.spec_buf[last].im = 0.0;
            let _ = self.ifft.process_with_scratch(
                &mut self.spec_buf,
                &mut self.out_buf,
                &mut self.scratch_inv,
            );

            for i in 0..n {
                let k = self.in_pos + i;
                output[done + i] = self.out_buf[k] + self.overlap[k];
            }

            self.in_pos += n;
            done += n;

            if self.in_pos == BLOCK {
                // The block is complete: keep the tail for the next block and rotate the FDL
                self.overlap.copy_from_slice(&self.out_buf[BLOCK..]);
                self.in_buf.iter_mut().for_each(|x| *x = 0.0);
                self.in_pos = 0;
                self.fdl_pos = (self.fdl_pos + 1) % self.num_parts;

                // Precompute partitions 1.. for the next block
                self.premult
                    .iter_mut()
                    .for_each(|c| *c = Complex32::new(0.0, 0.0));
                for p in 1..self.num_parts {
                    let idx = (self.fdl_pos + self.num_parts - p) % self.num_parts;
                    let x = &self.fdl[idx];
                    let h = &self.ir_parts[p];
                    for ((acc, &xv), &hv) in self.premult.iter_mut().zip(x.iter()).zip(h.iter()) {
                        *acc += xv * hv;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_direct_convolution() {
        let mut seed = 12345u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        let ir: Vec<f32> = (0..1700).map(|_| rnd()).collect();
        let input: Vec<f32> = (0..5000).map(|_| rnd()).collect();
        let mut expected = vec![0.0f32; input.len()];
        for (n, e) in expected.iter_mut().enumerate() {
            let mut acc = 0.0f64;
            for (k, &h) in ir.iter().enumerate() {
                if k <= n {
                    acc += h as f64 * input[n - k] as f64;
                }
            }
            *e = acc as f32;
        }
        let mut conv = Convolver::new(&ir);
        let mut output = vec![0.0f32; input.len()];
        // Odd chunk sizes to exercise partial blocks
        let mut pos = 0;
        let sizes = [1usize, 7, 300, 512, 513, 64, 1000];
        let mut s = 0;
        while pos < input.len() {
            let n = sizes[s % sizes.len()].min(input.len() - pos);
            conv.process(&input[pos..pos + n], &mut output[pos..pos + n]);
            pos += n;
            s += 1;
        }
        let max_err = expected
            .iter()
            .zip(output.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-3, "max error {max_err}");
    }
}
