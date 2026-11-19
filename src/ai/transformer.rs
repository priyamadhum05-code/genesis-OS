//! Quantized INT8 NEON Transformer Block (Stage 10B).
//!
//! Implements a 4-core SMP INT8 self-attention + 2-layer FFN transformer block:
//! - Sequence length: 16 tokens
//! - Model dimension: 64
//! - Feed-forward dimension: 128
//!
//! Linear projections (Q, K, V) and FFN layers utilize `neon_dot_product_64`
//! partitioned across 4 SMP cores (4 tokens per core) via `dispatch_smp_work`,
//! verified bit-exact against a scalar reference implementation.

use crate::ai::nn::{neon_dot_product_64, scalar_dot_product_64};

pub const TRANSFORMER_SEQ_LEN: usize = 16;
pub const TRANSFORMER_D_MODEL: usize = 64;
pub const TRANSFORMER_D_FF: usize = 128;
pub const TRANSFORMER_MAGIC: &[u8; 8] = b"GENESTF1";

/// Precomputed integer exponential lookup table for fixed-point softmax:
/// EXP_TABLE[diff] where diff in [0, 31], normalized so EXP_TABLE[0] = 256.
pub const EXP_TABLE: [u32; 32] = [
    256, 199, 155, 121, 94, 73, 57, 45,
    35, 27, 21, 16, 13, 10, 8, 6,
    5, 4, 3, 2, 2, 1, 1, 1,
    1, 0, 0, 0, 0, 0, 0, 0,
];

#[repr(C, align(64))]
pub struct SmpTransformerContext {
    // Model Inputs & Parameters
    pub input: [i8; 16 * 64],
    pub wq: [i8; 64 * 64],
    pub bq: [i32; 64],
    pub wk: [i8; 64 * 64],
    pub bk: [i32; 64],
    pub wv: [i8; 64 * 64],
    pub bv: [i32; 64],
    pub w_ff1: [i8; 128 * 64],
    pub b_ff1: [i32; 128],
    pub w_ff2: [i8; 64 * 128],
    pub b_ff2: [i32; 64],

    // Activations & State
    pub q: [i8; 16 * 64],
    pub k: [i8; 16 * 64],
    pub v: [i8; 16 * 64],
    pub attn_out: [i8; 16 * 64],
    pub x1: [i8; 16 * 64],
    pub h_ffn: [i8; 16 * 128],
    pub output: [i8; 16 * 64],
}

pub static mut SMP_TRANSFORMER_CTX: SmpTransformerContext = SmpTransformerContext {
    input: [0; 16 * 64],
    wq: [0; 64 * 64],
    bq: [0; 64],
    wk: [0; 64 * 64],
    bk: [0; 64],
    wv: [0; 64 * 64],
    bv: [0; 64],
    w_ff1: [0; 128 * 64],
    b_ff1: [0; 128],
    w_ff2: [0; 64 * 128],
    b_ff2: [0; 64],
    q: [0; 16 * 64],
    k: [0; 16 * 64],
    v: [0; 16 * 64],
    attn_out: [0; 16 * 64],
    x1: [0; 16 * 64],
    h_ffn: [0; 16 * 128],
    output: [0; 16 * 64],
};

/// Worker entry point dispatched across 4 SMP cores for Q, K, V projections.
/// Each core computes projections for 4 tokens (Core 0: 0..4, Core 1: 4..8, etc.).
#[no_mangle]
pub extern "C" fn smp_transformer_qkv_worker(cpu_id: u64, ctx_ptr: u64) {
    let ctx = unsafe { &mut *(ctx_ptr as *mut SmpTransformerContext) };
    let start_token = (cpu_id as usize) * 4;
    let end_token = start_token + 4;

    for t in start_token..end_token {
        let in_row = unsafe { &*(&ctx.input[t * 64] as *const i8 as *const [i8; 64]) };
        for d in 0..64 {
            let wq_row = unsafe { &*(&ctx.wq[d * 64] as *const i8 as *const [i8; 64]) };
            let acc_q = neon_dot_product_64(in_row, wq_row, ctx.bq[d]);
            ctx.q[t * 64 + d] = (acc_q >> 7).clamp(-128, 127) as i8;

            let wk_row = unsafe { &*(&ctx.wk[d * 64] as *const i8 as *const [i8; 64]) };
            let acc_k = neon_dot_product_64(in_row, wk_row, ctx.bk[d]);
            ctx.k[t * 64 + d] = (acc_k >> 7).clamp(-128, 127) as i8;

            let wv_row = unsafe { &*(&ctx.wv[d * 64] as *const i8 as *const [i8; 64]) };
            let acc_v = neon_dot_product_64(in_row, wv_row, ctx.bv[d]);
            ctx.v[t * 64 + d] = (acc_v >> 7).clamp(-128, 127) as i8;
        }
    }
}

/// Worker entry point dispatched across 4 SMP cores for Attention, Softmax, V aggregation,
/// Residual 1, and 2-layer FFN with Residual 2.
#[no_mangle]
pub extern "C" fn smp_transformer_attn_ffn_worker(cpu_id: u64, ctx_ptr: u64) {
    let ctx = unsafe { &mut *(ctx_ptr as *mut SmpTransformerContext) };
    let start_token = (cpu_id as usize) * 4;
    let end_token = start_token + 4;

    for t in start_token..end_token {
        let q_row = unsafe { &*(&ctx.q[t * 64] as *const i8 as *const [i8; 64]) };

        // 1. Attention scores S[t, s] = Q[t] . K[s] / 8
        let mut raw_scores = [0i32; 16];
        for s in 0..16 {
            let k_row = unsafe { &*(&ctx.k[s * 64] as *const i8 as *const [i8; 64]) };
            let dot = neon_dot_product_64(q_row, k_row, 0);
            raw_scores[s] = dot / 8;
        }

        // 2. Softmax over scores
        let mut max_s = raw_scores[0];
        for s in 1..16 {
            if raw_scores[s] > max_s {
                max_s = raw_scores[s];
            }
        }

        let mut weights = [0u32; 16];
        let mut sum_w = 0u32;
        for s in 0..16 {
            let diff = ((max_s - raw_scores[s]).clamp(0, 31)) as usize;
            weights[s] = EXP_TABLE[diff];
            sum_w += weights[s];
        }
        if sum_w == 0 {
            sum_w = 1;
        }

        let mut prob = [0i32; 16];
        for s in 0..16 {
            prob[s] = ((weights[s] as u64 * 128) / sum_w as u64) as i32;
        }

        // 3. Weighted sum of V + Residual 1
        for d in 0..64 {
            let mut acc = 0i32;
            for s in 0..16 {
                acc += prob[s] * (ctx.v[s * 64 + d] as i32);
            }
            let attn_val = (acc / 128).clamp(-128, 127) as i8;
            ctx.attn_out[t * 64 + d] = attn_val;

            let in_val = ctx.input[t * 64 + d] as i32;
            ctx.x1[t * 64 + d] = (in_val + attn_val as i32).clamp(-128, 127) as i8;
        }

        // 4. FFN Layer 1 (64 -> 128) + ReLU
        let x1_row = unsafe { &*(&ctx.x1[t * 64] as *const i8 as *const [i8; 64]) };
        for h in 0..128 {
            let w_row = unsafe { &*(&ctx.w_ff1[h * 64] as *const i8 as *const [i8; 64]) };
            let acc = neon_dot_product_64(x1_row, w_row, ctx.b_ff1[h]);
            ctx.h_ffn[t * 128 + h] = if acc > 0 { (acc >> 7).clamp(0, 127) as i8 } else { 0 };
        }

        // 5. FFN Layer 2 (128 -> 64) + Residual 2
        let h_first = unsafe { &*(&ctx.h_ffn[t * 128] as *const i8 as *const [i8; 64]) };
        let h_second = unsafe { &*(&ctx.h_ffn[t * 128 + 64] as *const i8 as *const [i8; 64]) };
        for d in 0..64 {
            let w_first = unsafe { &*(&ctx.w_ff2[d * 128] as *const i8 as *const [i8; 64]) };
            let w_second = unsafe { &*(&ctx.w_ff2[d * 128 + 64] as *const i8 as *const [i8; 64]) };
            let acc1 = neon_dot_product_64(h_first, w_first, ctx.b_ff2[d]);
            let acc = neon_dot_product_64(h_second, w_second, acc1);
            let ffn_out = (acc >> 7).clamp(-128, 127) as i8;

            let x1_val = ctx.x1[t * 64 + d] as i32;
            ctx.output[t * 64 + d] = (x1_val + ffn_out as i32).clamp(-128, 127) as i8;
        }
    }
}

/// Runs full 4-core parallel INT8 Transformer block.
pub unsafe fn run_parallel_transformer(ctx: &mut SmpTransformerContext) -> [i8; 16 * 64] {
    let ctx_ptr = ctx as *mut SmpTransformerContext as u64;

    #[cfg(target_os = "uefi")]
    {
        crate::arch::dispatch_smp_work(smp_transformer_qkv_worker, ctx_ptr);
        crate::arch::dispatch_smp_work(smp_transformer_attn_ffn_worker, ctx_ptr);
    }
    #[cfg(not(target_os = "uefi"))]
    {
        for cpu in 0..4 {
            smp_transformer_qkv_worker(cpu, ctx_ptr);
        }
        for cpu in 0..4 {
            smp_transformer_attn_ffn_worker(cpu, ctx_ptr);
        }
    }

    ctx.output
}

/// Exact scalar reference implementation of the complete Transformer block.
pub fn scalar_transformer_block(
    input: &[i8; 16 * 64],
    wq: &[i8; 64 * 64],
    bq: &[i32; 64],
    wk: &[i8; 64 * 64],
    bk: &[i32; 64],
    wv: &[i8; 64 * 64],
    bv: &[i32; 64],
    w_ff1: &[i8; 128 * 64],
    b_ff1: &[i32; 128],
    w_ff2: &[i8; 64 * 128],
    b_ff2: &[i32; 64],
) -> [i8; 16 * 64] {
    let mut q = [0i8; 16 * 64];
    let mut k = [0i8; 16 * 64];
    let mut v = [0i8; 16 * 64];

    // Projections
    for t in 0..16 {
        let in_row = unsafe { &*(&input[t * 64] as *const i8 as *const [i8; 64]) };
        for d in 0..64 {
            let wq_row = unsafe { &*(&wq[d * 64] as *const i8 as *const [i8; 64]) };
            let acc_q = scalar_dot_product_64(in_row, wq_row, bq[d]);
            q[t * 64 + d] = (acc_q >> 7).clamp(-128, 127) as i8;

            let wk_row = unsafe { &*(&wk[d * 64] as *const i8 as *const [i8; 64]) };
            let acc_k = scalar_dot_product_64(in_row, wk_row, bk[d]);
            k[t * 64 + d] = (acc_k >> 7).clamp(-128, 127) as i8;

            let wv_row = unsafe { &*(&wv[d * 64] as *const i8 as *const [i8; 64]) };
            let acc_v = scalar_dot_product_64(in_row, wv_row, bv[d]);
            v[t * 64 + d] = (acc_v >> 7).clamp(-128, 127) as i8;
        }
    }

    let mut x1 = [0i8; 16 * 64];
    let mut h_ffn = [0i8; 16 * 128];
    let mut output = [0i8; 16 * 64];

    for t in 0..16 {
        let q_row = unsafe { &*(&q[t * 64] as *const i8 as *const [i8; 64]) };

        let mut raw_scores = [0i32; 16];
        for s in 0..16 {
            let k_row = unsafe { &*(&k[s * 64] as *const i8 as *const [i8; 64]) };
            let dot = scalar_dot_product_64(q_row, k_row, 0);
            raw_scores[s] = dot / 8;
        }

        let mut max_s = raw_scores[0];
        for s in 1..16 {
            if raw_scores[s] > max_s {
                max_s = raw_scores[s];
            }
        }

        let mut weights = [0u32; 16];
        let mut sum_w = 0u32;
        for s in 0..16 {
            let diff = ((max_s - raw_scores[s]).clamp(0, 31)) as usize;
            weights[s] = EXP_TABLE[diff];
            sum_w += weights[s];
        }
        if sum_w == 0 {
            sum_w = 1;
        }

        let mut prob = [0i32; 16];
        for s in 0..16 {
            prob[s] = ((weights[s] as u64 * 128) / sum_w as u64) as i32;
        }

        for d in 0..64 {
            let mut acc = 0i32;
            for s in 0..16 {
                acc += prob[s] * (v[s * 64 + d] as i32);
            }
            let attn_val = (acc / 128).clamp(-128, 127) as i8;

            let in_val = input[t * 64 + d] as i32;
            x1[t * 64 + d] = (in_val + attn_val as i32).clamp(-128, 127) as i8;
        }

        let x1_row = unsafe { &*(&x1[t * 64] as *const i8 as *const [i8; 64]) };
        for h in 0..128 {
            let w_row = unsafe { &*(&w_ff1[h * 64] as *const i8 as *const [i8; 64]) };
            let acc = scalar_dot_product_64(x1_row, w_row, b_ff1[h]);
            h_ffn[t * 128 + h] = if acc > 0 { (acc >> 7).clamp(0, 127) as i8 } else { 0 };
        }

        let h_first = unsafe { &*(&h_ffn[t * 128] as *const i8 as *const [i8; 64]) };
        let h_second = unsafe { &*(&h_ffn[t * 128 + 64] as *const i8 as *const [i8; 64]) };
        for d in 0..64 {
            let w_first = unsafe { &*(&w_ff2[d * 128] as *const i8 as *const [i8; 64]) };
            let w_second = unsafe { &*(&w_ff2[d * 128 + 64] as *const i8 as *const [i8; 64]) };
            let acc1 = scalar_dot_product_64(h_first, w_first, b_ff2[d]);
            let acc = scalar_dot_product_64(h_second, w_second, acc1);
            let ffn_out = (acc >> 7).clamp(-128, 127) as i8;

            let x1_val = x1[t * 64 + d] as i32;
            output[t * 64 + d] = (x1_val + ffn_out as i32).clamp(-128, 127) as i8;
        }
    }

    output
}

/// Verifies parallel NEON Transformer output against exact scalar reference.
pub fn verify_transformer(parallel_out: &[i8; 16 * 64], scalar_out: &[i8; 16 * 64]) -> bool {
    parallel_out == scalar_out
}

/// Parses and loads a quantized transformer model file into `ctx`.
pub unsafe fn load_transformer_from_bytes(
    bytes: &[u8],
    ctx: &mut SmpTransformerContext,
) -> Result<(), &'static str> {
    if bytes.len() < 31264 {
        return Err("Transformer file too small for model parameters");
    }

    if &bytes[0..8] != TRANSFORMER_MAGIC {
        return Err("Invalid transformer model magic");
    }

    let seq_len = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    let d_model = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;
    let d_ff = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]) as usize;

    if seq_len != TRANSFORMER_SEQ_LEN || d_model != TRANSFORMER_D_MODEL || d_ff != TRANSFORMER_D_FF {
        return Err("Transformer dimension mismatch");
    }

    let mut cursor = 32usize;

    // input: 16 * 64
    let in_bytes = core::slice::from_raw_parts(bytes.as_ptr().add(cursor) as *const i8, 16 * 64);
    ctx.input.copy_from_slice(in_bytes);
    cursor += 16 * 64;

    // wq: 64 * 64
    let wq_bytes = core::slice::from_raw_parts(bytes.as_ptr().add(cursor) as *const i8, 64 * 64);
    ctx.wq.copy_from_slice(wq_bytes);
    cursor += 64 * 64;

    // bq: 64 * 4
    for i in 0..64 {
        ctx.bq[i] = i32::from_le_bytes([bytes[cursor], bytes[cursor + 1], bytes[cursor + 2], bytes[cursor + 3]]);
        cursor += 4;
    }

    // wk: 64 * 64
    let wk_bytes = core::slice::from_raw_parts(bytes.as_ptr().add(cursor) as *const i8, 64 * 64);
    ctx.wk.copy_from_slice(wk_bytes);
    cursor += 64 * 64;

    // bk: 64 * 4
    for i in 0..64 {
        ctx.bk[i] = i32::from_le_bytes([bytes[cursor], bytes[cursor + 1], bytes[cursor + 2], bytes[cursor + 3]]);
        cursor += 4;
    }

    // wv: 64 * 64
    let wv_bytes = core::slice::from_raw_parts(bytes.as_ptr().add(cursor) as *const i8, 64 * 64);
    ctx.wv.copy_from_slice(wv_bytes);
    cursor += 64 * 64;

    // bv: 64 * 4
    for i in 0..64 {
        ctx.bv[i] = i32::from_le_bytes([bytes[cursor], bytes[cursor + 1], bytes[cursor + 2], bytes[cursor + 3]]);
        cursor += 4;
    }

    // w_ff1: 128 * 64
    let wff1_bytes = core::slice::from_raw_parts(bytes.as_ptr().add(cursor) as *const i8, 128 * 64);
    ctx.w_ff1.copy_from_slice(wff1_bytes);
    cursor += 128 * 64;

    // b_ff1: 128 * 4
    for i in 0..128 {
        ctx.b_ff1[i] = i32::from_le_bytes([bytes[cursor], bytes[cursor + 1], bytes[cursor + 2], bytes[cursor + 3]]);
        cursor += 4;
    }

    // w_ff2: 64 * 128
    let wff2_bytes = core::slice::from_raw_parts(bytes.as_ptr().add(cursor) as *const i8, 64 * 128);
    ctx.w_ff2.copy_from_slice(wff2_bytes);
    cursor += 64 * 128;

    // b_ff2: 64 * 4
    for i in 0..64 {
        ctx.b_ff2[i] = i32::from_le_bytes([bytes[cursor], bytes[cursor + 1], bytes[cursor + 2], bytes[cursor + 3]]);
        cursor += 4;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::boxed::Box;

    #[test]
    fn full_transformer_neon_vs_scalar_equivalence() {
        let mut ctx = Box::new(SmpTransformerContext {
            input: [0; 16 * 64],
            wq: [0; 64 * 64],
            bq: [0; 64],
            wk: [0; 64 * 64],
            bk: [0; 64],
            wv: [0; 64 * 64],
            bv: [0; 64],
            w_ff1: [0; 128 * 64],
            b_ff1: [0; 128],
            w_ff2: [0; 64 * 128],
            b_ff2: [0; 64],
            q: [0; 16 * 64],
            k: [0; 16 * 64],
            v: [0; 16 * 64],
            attn_out: [0; 16 * 64],
            x1: [0; 16 * 64],
            h_ffn: [0; 16 * 128],
            output: [0; 16 * 64],
        });

        // Initialize pseudo-random deterministic test weights
        for t in 0..16 {
            for d in 0..64 {
                ctx.input[t * 64 + d] = (((t * 5 + d * 3) % 19) as i8) - 9;
            }
        }
        for d in 0..64 {
            ctx.bq[d] = ((d * 11) % 40) as i32 - 20;
            ctx.bk[d] = ((d * 7) % 30) as i32 - 15;
            ctx.bv[d] = ((d * 5) % 25) as i32 - 12;
            ctx.b_ff2[d] = ((d * 13) % 35) as i32 - 17;
            for i in 0..64 {
                ctx.wq[d * 64 + i] = (((d + i) * 7) % 13) as i8 - 6;
                ctx.wk[d * 64 + i] = (((d * 3 + i * 5) % 11) as i8) - 5;
                ctx.wv[d * 64 + i] = (((d * 2 + i * 7) % 17) as i8) - 8;
            }
        }
        for h in 0..128 {
            ctx.b_ff1[h] = ((h * 9) % 50) as i32 - 25;
            for i in 0..64 {
                ctx.w_ff1[h * 64 + i] = (((h + i) * 3) % 15) as i8 - 7;
            }
            for d in 0..64 {
                ctx.w_ff2[d * 128 + h] = (((d + h * 3) % 11) as i8) - 5;
            }
        }

        let scalar_out = scalar_transformer_block(
            &ctx.input,
            &ctx.wq,
            &ctx.bq,
            &ctx.wk,
            &ctx.bk,
            &ctx.wv,
            &ctx.bv,
            &ctx.w_ff1,
            &ctx.b_ff1,
            &ctx.w_ff2,
            &ctx.b_ff2,
        );

        let parallel_out = unsafe { run_parallel_transformer(&mut ctx) };

        assert_eq!(
            parallel_out, scalar_out,
            "Parallel NEON Transformer must match scalar reference bit-for-bit"
        );
    }
}
