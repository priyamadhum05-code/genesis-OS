//! Multi-Layer INT8 Quantized NEON Neural-Net Inference (Stage 9C).
//!
//! Implements a 2-layer INT8 quantized feed-forward network:
//! Input[64] -> Linear1(64x64, i8) -> ReLU -> Linear2(64x10, i8) -> Argmax
//! using AArch64 NEON integer widening multiply-accumulate (smull/saddw),
//! with Layer 1 partitioned across the 4 SMP cores via `dispatch_smp_work`,
//! verified against an exact scalar reference.

pub const NN_INPUT_SIZE: usize = 64;
pub const NN_HIDDEN_SIZE: usize = 64;
pub const NN_OUTPUT_SIZE: usize = 10;
pub const NN_MODEL_MAGIC: &[u8; 8] = b"GENESNN1";

#[repr(C, align(64))]
pub struct SmpNnContext {
    pub input: [i8; 64],
    pub w1: [i8; 64 * 64],
    pub b1: [i32; 64],
    pub h1: [i8; 64],
    pub w2: [i8; 10 * 64],
    pub b2: [i32; 10],
    pub logits: [i32; 10],
}

pub static mut SMP_NN_CTX: SmpNnContext = SmpNnContext {
    input: [0; 64],
    w1: [0; 64 * 64],
    b1: [0; 64],
    h1: [0; 64],
    w2: [0; 10 * 64],
    b2: [0; 10],
    logits: [0; 10],
};

/// Computes the dot product of two 64-element i8 vectors using NEON SIMD,
/// widening i8 -> i16 -> i32 and adding `bias`.
#[inline]
pub fn neon_dot_product_64(input: &[i8; 64], weights: &[i8; 64], bias: i32) -> i32 {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        let sum: i32;
        core::arch::asm!(
            "movi   v4.4s, #0",

            // Block 0 (elements 0..15)
            "ldr    q0, [{in_ptr}, #0]",
            "ldr    q1, [{w_ptr}, #0]",
            "smull  v2.8h, v0.8b, v1.8b",
            "smull2 v3.8h, v0.16b, v1.16b",
            "saddw  v4.4s, v4.4s, v2.4h",
            "saddw2 v4.4s, v4.4s, v2.8h",
            "saddw  v4.4s, v4.4s, v3.4h",
            "saddw2 v4.4s, v4.4s, v3.8h",

            // Block 1 (elements 16..31)
            "ldr    q0, [{in_ptr}, #16]",
            "ldr    q1, [{w_ptr}, #16]",
            "smull  v2.8h, v0.8b, v1.8b",
            "smull2 v3.8h, v0.16b, v1.16b",
            "saddw  v4.4s, v4.4s, v2.4h",
            "saddw2 v4.4s, v4.4s, v2.8h",
            "saddw  v4.4s, v4.4s, v3.4h",
            "saddw2 v4.4s, v4.4s, v3.8h",

            // Block 2 (elements 32..47)
            "ldr    q0, [{in_ptr}, #32]",
            "ldr    q1, [{w_ptr}, #32]",
            "smull  v2.8h, v0.8b, v1.8b",
            "smull2 v3.8h, v0.16b, v1.16b",
            "saddw  v4.4s, v4.4s, v2.4h",
            "saddw2 v4.4s, v4.4s, v2.8h",
            "saddw  v4.4s, v4.4s, v3.4h",
            "saddw2 v4.4s, v4.4s, v3.8h",

            // Block 3 (elements 48..63)
            "ldr    q0, [{in_ptr}, #48]",
            "ldr    q1, [{w_ptr}, #48]",
            "smull  v2.8h, v0.8b, v1.8b",
            "smull2 v3.8h, v0.16b, v1.16b",
            "saddw  v4.4s, v4.4s, v2.4h",
            "saddw2 v4.4s, v4.4s, v2.8h",
            "saddw  v4.4s, v4.4s, v3.4h",
            "saddw2 v4.4s, v4.4s, v3.8h",

            // Across vector horizontal sum
            "addv   s4, v4.4s",
            "fmov   {sum:w}, s4",

            in_ptr = in(reg) input.as_ptr(),
            w_ptr = in(reg) weights.as_ptr(),
            sum = out(reg) sum,
            out("v0") _,
            out("v1") _,
            out("v2") _,
            out("v3") _,
            out("v4") _,
            options(nostack, preserves_flags),
        );
        sum.wrapping_add(bias)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        scalar_dot_product_64(input, weights, bias)
    }
}

/// Exact scalar reference implementation of the 64-element dot product.
#[inline]
pub fn scalar_dot_product_64(input: &[i8; 64], weights: &[i8; 64], bias: i32) -> i32 {
    let mut acc = bias;
    for j in 0..64 {
        acc = acc.wrapping_add((input[j] as i32) * (weights[j] as i32));
    }
    acc
}

/// Worker entry point dispatched across the 4 SMP cores for Layer 1.
/// Each core computes 16 hidden neurons (Core 0: 0..16, Core 1: 16..32, etc.).
#[no_mangle]
pub extern "C" fn smp_nn_layer1_worker(cpu_id: u64, ctx_ptr: u64) {
    let ctx = unsafe { &mut *(ctx_ptr as *mut SmpNnContext) };
    let start_neuron = (cpu_id as usize) * 16;
    let end_neuron = start_neuron + 16;

    for i in start_neuron..end_neuron {
        let w_row = unsafe { &*(&ctx.w1[i * 64] as *const i8 as *const [i8; 64]) };
        let acc = neon_dot_product_64(&ctx.input, w_row, ctx.b1[i]);
        // ReLU activation clamped to [0, 127]
        ctx.h1[i] = if acc > 0 { acc.min(127) as i8 } else { 0 };
    }
}

/// Runs full 2-layer neural network inference with Layer 1 parallelized on 4 cores.
/// Returns `(predicted_class, logits)`.
pub unsafe fn run_parallel_nn_inference(ctx: &mut SmpNnContext) -> (usize, [i32; 10]) {
    let ctx_ptr = ctx as *mut SmpNnContext as u64;

    // Dispatch Layer 1 across 4 SMP cores
    #[cfg(target_os = "uefi")]
    {
        crate::arch::dispatch_smp_work(smp_nn_layer1_worker, ctx_ptr);
    }
    #[cfg(not(target_os = "uefi"))]
    {
        for cpu in 0..4 {
            smp_nn_layer1_worker(cpu, ctx_ptr);
        }
    }

    // Layer 2: 10 output classes (run on core 0)
    for k in 0..10 {
        let w_row = unsafe { &*(&ctx.w2[k * 64] as *const i8 as *const [i8; 64]) };
        ctx.logits[k] = neon_dot_product_64(&ctx.h1, w_row, ctx.b2[k]);
    }

    // Argmax
    let mut best_class = 0;
    let mut max_logit = ctx.logits[0];
    for k in 1..10 {
        if ctx.logits[k] > max_logit {
            max_logit = ctx.logits[k];
            best_class = k;
        }
    }

    (best_class, ctx.logits)
}

/// Exact scalar reference for full 2-layer network.
pub fn scalar_infer(
    input: &[i8; 64],
    w1: &[i8; 64 * 64],
    b1: &[i32; 64],
    w2: &[i8; 10 * 64],
    b2: &[i32; 10],
) -> (usize, [i32; 10]) {
    let mut h1 = [0i8; 64];
    for i in 0..64 {
        let w_row = unsafe { &*(&w1[i * 64] as *const i8 as *const [i8; 64]) };
        let acc = scalar_dot_product_64(input, w_row, b1[i]);
        h1[i] = if acc > 0 { acc.min(127) as i8 } else { 0 };
    }

    let mut logits = [0i32; 10];
    for k in 0..10 {
        let w_row = unsafe { &*(&w2[k * 64] as *const i8 as *const [i8; 64]) };
        logits[k] = scalar_dot_product_64(&h1, w_row, b2[k]);
    }

    let mut best_class = 0;
    let mut max_logit = logits[0];
    for k in 1..10 {
        if logits[k] > max_logit {
            max_logit = logits[k];
            best_class = k;
        }
    }

    (best_class, logits)
}

/// Verifies that NEON output matches the scalar reference exactly.
pub fn verify_inference(
    neon_class: usize,
    neon_logits: &[i32; 10],
    scalar_class: usize,
    scalar_logits: &[i32; 10],
) -> bool {
    neon_class == scalar_class && neon_logits == scalar_logits
}

/// Loads model weights, biases, and input vector from a byte buffer.
pub fn load_model_from_bytes(bytes: &[u8], ctx: &mut SmpNnContext) -> Result<(), &'static str> {
    let off = if bytes.len() >= 5120 && &bytes[0..8] == NN_MODEL_MAGIC {
        24 // 8 (magic) + 4 (in) + 4 (hidden) + 4 (out) + 4 (reserved)
    } else if bytes.len() >= 5096 {
        0
    } else {
        return Err("Model file too small (< 5096 bytes)");
    };

    let required = off + 64 + 64 * 64 + 64 * 4 + 10 * 64 + 10 * 4;
    if bytes.len() < required {
        return Err("Model file truncated");
    }

    unsafe {
        let mut p = bytes.as_ptr().add(off);
        core::ptr::copy_nonoverlapping(p as *const i8, ctx.input.as_mut_ptr(), 64);
        p = p.add(64);
        core::ptr::copy_nonoverlapping(p as *const i8, ctx.w1.as_mut_ptr(), 64 * 64);
        p = p.add(64 * 64);
        core::ptr::copy_nonoverlapping(p as *const i32, ctx.b1.as_mut_ptr(), 64);
        p = p.add(64 * 4);
        core::ptr::copy_nonoverlapping(p as *const i8, ctx.w2.as_mut_ptr(), 10 * 64);
        p = p.add(10 * 64);
        core::ptr::copy_nonoverlapping(p as *const i32, ctx.b2.as_mut_ptr(), 10);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neon_and_scalar_dot_product_equivalence() {
        let mut input = [0i8; 64];
        let mut weights = [0i8; 64];
        for i in 0..64 {
            input[i] = (i as i8).wrapping_mul(3).wrapping_sub(50);
            weights[i] = ((63 - i) as i8).wrapping_mul(2).wrapping_sub(40);
        }
        let bias = 12345;

        let scalar_res = scalar_dot_product_64(&input, &weights, bias);
        let neon_res = neon_dot_product_64(&input, &weights, bias);
        assert_eq!(neon_res, scalar_res);
    }

    #[test]
    fn full_model_neon_vs_scalar_equivalence() {
        let mut ctx = SmpNnContext {
            input: [0; 64],
            w1: [0; 64 * 64],
            b1: [0; 64],
            h1: [0; 64],
            w2: [0; 10 * 64],
            b2: [0; 10],
            logits: [0; 10],
        };

        for i in 0..64 {
            ctx.input[i] = ((i as i8) % 15) - 7;
            ctx.b1[i] = ((i as i32) * 11) - 200;
            for j in 0..64 {
                ctx.w1[i * 64 + j] = (((i + j) as i8) % 9) - 4;
            }
        }

        for k in 0..10 {
            ctx.b2[k] = ((k as i32) * 50) - 100;
            for i in 0..64 {
                ctx.w2[k * 64 + i] = (((k * 7 + i) as i8) % 11) - 5;
            }
        }

        let (scalar_class, scalar_logits) = scalar_infer(&ctx.input, &ctx.w1, &ctx.b1, &ctx.w2, &ctx.b2);
        let (neon_class, neon_logits) = unsafe { run_parallel_nn_inference(&mut ctx) };

        assert_eq!(neon_class, scalar_class);
        assert_eq!(neon_logits, scalar_logits);
        assert!(verify_inference(neon_class, &neon_logits, scalar_class, &scalar_logits));
    }
}
