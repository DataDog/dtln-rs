// dtln_engine.rs
use std::ptr;
use std::slice;

use anyhow::Result;
use num::Complex;
use realfft::RealFftPlanner;

use crate::constants::*;
use crate::tflite::*;

pub struct DtlnEngine {
    model1: *const TfLiteModel,
    interpreter_1: *mut TfLiteInterpreter,
    model2: *const TfLiteModel,
    interpreter_2: *mut TfLiteInterpreter,
    details1: [*mut TfLiteTensor; 2],
    output_details_1: [*const TfLiteTensor; 2],
    details2: [*mut TfLiteTensor; 2],
    output_details_2: [*const TfLiteTensor; 2],
    valid: bool,
    in_buffer: [f32; DTLN_BLOCK_LEN],
    out_buffer: [f32; DTLN_BLOCK_LEN],
    states_1: [f32; DTLN_BLOCK_LEN],
    states_2: [f32; DTLN_BLOCK_LEN],
}

unsafe impl Send for DtlnEngine {}

impl DtlnEngine {
    pub fn new() -> Option<Self> {
        let model1_data = include_bytes!("../model/model_quant_1.tflite");
        let model1_size = model1_data.len();

        let model1 = unsafe { TfLiteModelCreate(model1_data.as_ptr() as *const _, model1_size) };
        if model1.is_null() {
            eprintln!("[DTLN] Failed to create model 1");
            return None;
        }

        let options = unsafe { TfLiteInterpreterOptionsCreate() };
        unsafe { TfLiteInterpreterOptionsSetNumThreads(options, 1) };

        let interpreter_1 = unsafe { TfLiteInterpreterCreate(model1, options) };
        if interpreter_1.is_null() {
            eprintln!("[DTLN] Failed to create interpreter for DTLN model 1");
            unsafe { TfLiteModelDelete(model1) };
            return None;
        }

        if unsafe { TfLiteInterpreterAllocateTensors(interpreter_1) }
            .to_result()
            .is_err()
        {
            eprintln!("[DTLN] Failed to allocate tensors for DTLN model 1");
            unsafe {
                TfLiteInterpreterDelete(interpreter_1);
                TfLiteModelDelete(model1);
            }
            return None;
        }

        let model2_data = include_bytes!("../model/model_quant_2.tflite");
        let model2_size = model2_data.len();

        let model2 = unsafe { TfLiteModelCreate(model2_data.as_ptr() as *const _, model2_size) };
        if model2.is_null() {
            eprintln!("[DTLN] Failed to create model 2");
            unsafe {
                TfLiteInterpreterDelete(interpreter_1);
                TfLiteModelDelete(model1);
            }
            return None;
        }

        let interpreter_2 = unsafe { TfLiteInterpreterCreate(model2, options) };
        if interpreter_2.is_null() {
            eprintln!("[DTLN] Failed to create interpreter for DTLN model 2");
            unsafe {
                TfLiteInterpreterDelete(interpreter_1);
                TfLiteModelDelete(model1);
                TfLiteModelDelete(model2);
            }
            return None;
        }

        if unsafe { TfLiteInterpreterAllocateTensors(interpreter_2) }
            .to_result()
            .is_err()
        {
            eprintln!("[DTLN] Failed to allocate tensors for DTLN model 2");
            unsafe {
                TfLiteInterpreterDelete(interpreter_1);
                TfLiteInterpreterDelete(interpreter_2);
                TfLiteModelDelete(model1);
                TfLiteModelDelete(model2);
            }
            return None;
        }

        let details1_0 = unsafe { TfLiteInterpreterGetInputTensor(interpreter_1, 0) };
        let details1_1 = unsafe { TfLiteInterpreterGetInputTensor(interpreter_1, 1) };
        let output_details_1_0 = unsafe { TfLiteInterpreterGetOutputTensor(interpreter_1, 0) };
        let output_details_1_1 = unsafe { TfLiteInterpreterGetOutputTensor(interpreter_1, 1) };

        let details2_0 = unsafe { TfLiteInterpreterGetInputTensor(interpreter_2, 0) };
        let details2_1 = unsafe { TfLiteInterpreterGetInputTensor(interpreter_2, 1) };
        let output_details_2_0 = unsafe { TfLiteInterpreterGetOutputTensor(interpreter_2, 0) };
        let output_details_2_1 = unsafe { TfLiteInterpreterGetOutputTensor(interpreter_2, 1) };

        unsafe { TfLiteInterpreterOptionsDelete(options) };

        Some(DtlnEngine {
            model1,
            interpreter_1,
            model2,
            interpreter_2,
            details1: [details1_0, details1_1],
            output_details_1: [output_details_1_0, output_details_1_1],
            details2: [details2_0, details2_1],
            output_details_2: [output_details_2_0, output_details_2_1],
            valid: true,
            in_buffer: [0.0; DTLN_BLOCK_LEN],
            out_buffer: [0.0; DTLN_BLOCK_LEN],
            states_1: [0.0; DTLN_BLOCK_LEN],
            states_2: [0.0; DTLN_BLOCK_LEN],
        })
    }

    pub fn denoise(&mut self, samples: &[f32], out: &mut [f32]) {
        let sample_count = samples.len();
        let num_blocks = sample_count / DTLN_BLOCK_SHIFT;
        assert!(out.len() >= sample_count);

        for idx in 0..num_blocks {
            // Shift in_buffer left by DTLN_BLOCK_SHIFT samples
            self.in_buffer.copy_within(DTLN_BLOCK_SHIFT.., 0);

            // Copy next DTLN_BLOCK_SHIFT samples into in_buffer
            self.in_buffer[(DTLN_BLOCK_LEN - DTLN_BLOCK_SHIFT)..]
                .copy_from_slice(&samples[idx * DTLN_BLOCK_SHIFT..(idx + 1) * DTLN_BLOCK_SHIFT]);

            self.infer();

            // Copy DTLN_BLOCK_SHIFT samples from out_buffer to out
            out[idx * DTLN_BLOCK_SHIFT..(idx + 1) * DTLN_BLOCK_SHIFT]
                .copy_from_slice(&self.out_buffer[..DTLN_BLOCK_SHIFT]);
        }
    }

    fn infer(&mut self) {
        if !self.valid {
            eprintln!("[DTLN] Engine not initialized");
            return;
        }

        let mut in_mag = [0f32; DTLN_FFT_OUT_SIZE];
        let mut in_phase = [0f32; DTLN_FFT_OUT_SIZE];
        let mut estimated_block = [0f32; DTLN_BLOCK_LEN];

        // Prepare FFT input
        let mut planner = RealFftPlanner::<f32>::new();
        let r2c = planner.plan_fft_forward(DTLN_BLOCK_LEN);
        let mut fft_in = self.in_buffer.to_vec();
        let mut fft_spectrum = r2c.make_output_vec();

        // Perform real-to-complex FFT
        r2c.process(&mut fft_in, &mut fft_spectrum).unwrap();

        // Generate magnitude and phase
        for i in 0..DTLN_FFT_OUT_SIZE {
            in_mag[i] = fft_spectrum[i].norm();
            in_phase[i] = fft_spectrum[i].arg();
        }

        // Prepare inputs for model 1
        let in_mag_ptr = unsafe { TfLiteTensorData(self.details1[0]) as *mut f32 };
        unsafe {
            ptr::copy_nonoverlapping(in_mag.as_ptr(), in_mag_ptr, DTLN_FFT_OUT_SIZE);
        }
        let states1_ptr = unsafe { TfLiteTensorData(self.details1[1]) as *mut f32 };
        unsafe {
            ptr::copy_nonoverlapping(self.states_1.as_ptr(), states1_ptr, DTLN_BLOCK_LEN);
        }

        // Invoke model 1
        if unsafe { TfLiteInterpreterInvoke(self.interpreter_1) }
            .to_result()
            .is_err()
        {
            eprintln!("[DTLN] Failed to invoke interpreter for model 1");
            return;
        }

        // Get outputs
        let out_mask_ptr = unsafe { TfLiteTensorData(self.output_details_1[0]) as *const f32 };
        let out_mask = unsafe { slice::from_raw_parts(out_mask_ptr, DTLN_FFT_OUT_SIZE) };

        let out_states1_ptr = unsafe { TfLiteTensorData(self.output_details_1[1]) as *const f32 };
        unsafe {
            ptr::copy_nonoverlapping(out_states1_ptr, self.states_1.as_mut_ptr(), DTLN_BLOCK_LEN);
        }

        // Apply mask and reconstruct complex spectrum
        for i in 0..DTLN_FFT_OUT_SIZE {
            let magnitude = in_mag[i] * out_mask[i];
            let phase = in_phase[i];
            let real = magnitude * phase.cos();
            let imag = magnitude * phase.sin();
            fft_spectrum[i] = Complex::new(real, imag);
        }

        // Handle DC component (i = 0)
        let magnitude = in_mag[0] * out_mask[0];
        fft_spectrum[0] = Complex::new(magnitude, 0.0);

        // Handle Nyquist component (i = N/2)
        let magnitude = in_mag[DTLN_FFT_OUT_SIZE - 1] * out_mask[DTLN_FFT_OUT_SIZE - 1];
        fft_spectrum[DTLN_FFT_OUT_SIZE - 1] = Complex::new(magnitude, 0.0);

        // Prepare for inverse FFT
        let c2r = planner.plan_fft_inverse(DTLN_BLOCK_LEN);
        let mut ifft_output = c2r.make_output_vec();

        // Perform complex-to-real IFFT
        c2r.process(&mut fft_spectrum, &mut ifft_output).unwrap();

        // Normalize the IFFT output
        for i in 0..DTLN_BLOCK_LEN {
            estimated_block[i] = ifft_output[i] / DTLN_BLOCK_LEN as f32;
        }

        // Prepare inputs for model 2
        let est_block_ptr = unsafe { TfLiteTensorData(self.details2[0]) as *mut f32 };
        unsafe {
            ptr::copy_nonoverlapping(estimated_block.as_ptr(), est_block_ptr, DTLN_BLOCK_LEN);
        }
        let states2_ptr = unsafe { TfLiteTensorData(self.details2[1]) as *mut f32 };
        unsafe {
            ptr::copy_nonoverlapping(self.states_2.as_ptr(), states2_ptr, DTLN_BLOCK_LEN);
        }

        // Invoke model 2
        if unsafe { TfLiteInterpreterInvoke(self.interpreter_2) }
            .to_result()
            .is_err()
        {
            eprintln!("[DTLN] Failed to invoke interpreter for model 2");
            return;
        }

        // Get outputs
        let out_block_ptr = unsafe { TfLiteTensorData(self.output_details_2[0]) as *const f32 };
        let out_block = unsafe { slice::from_raw_parts(out_block_ptr, DTLN_BLOCK_LEN) };

        let out_states2_ptr = unsafe { TfLiteTensorData(self.output_details_2[1]) as *const f32 };
        unsafe {
            ptr::copy_nonoverlapping(out_states2_ptr, self.states_2.as_mut_ptr(), DTLN_BLOCK_LEN);
        }

        // Overlap-add
        self.out_buffer.copy_within(DTLN_BLOCK_SHIFT.., 0);
        for i in (DTLN_BLOCK_LEN - DTLN_BLOCK_SHIFT)..DTLN_BLOCK_LEN {
            self.out_buffer[i] = 0.0;
        }

        for (i, item) in out_block.iter().enumerate().take(DTLN_BLOCK_LEN) {
            self.out_buffer[i] += item;
        }
    }
}

impl Drop for DtlnEngine {
    fn drop(&mut self) {
        unsafe {
            TfLiteInterpreterDelete(self.interpreter_1);
            TfLiteInterpreterDelete(self.interpreter_2);
            TfLiteModelDelete(self.model1);
            TfLiteModelDelete(self.model2);
        }
    }
}

pub fn dtln_create() -> Option<DtlnEngine> {
    DtlnEngine::new()
}

pub fn dtln_denoise(engine: &mut DtlnEngine, samples: &[f32], out: &mut [f32]) -> Result<()> {
    if out.len() < samples.len() {
        eprintln!(
            "[DTLN] Output buffer too small, {} vs {}",
            out.len(),
            samples.len()
        );
        return Err(anyhow::anyhow!("Output buffer too small"));
    }
    engine.denoise(samples, out);
    Ok(())
}
