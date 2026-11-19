//! Quantized INT8 4-Core Autoregressive Micro-LLM (Stage 11B).
//!
//! Implements a compact byte-level autoregressive language model:
//! - Vocabulary: 128 (7-bit ASCII)
//! - Model dimension: 64
//! - Feed-forward dimension: 128
//! - Layers: 2 Transformer blocks
//! - Sequence length: 16 tokens
//!
//! Attention and feed-forward projections run in parallel across 4 SMP cores
//! using NEON SIMD kernels (`neon_dot_product_64`), followed by an LM head projection
//! and greedy argmax token sampler. Bit-exact equivalence against a scalar reference
//! is verified on every step.

use crate::ai::nn::{neon_dot_product_64, scalar_dot_product_64};
use crate::ai::transformer::{
    run_parallel_transformer, scalar_transformer_block, SmpTransformerContext,
};

pub const MICRO_LLM_VOCAB: usize = 128;
pub const MICRO_LLM_D_MODEL: usize = 64;
pub const MICRO_LLM_D_FF: usize = 128;
pub const MICRO_LLM_SEQ_LEN: usize = 16;
pub const MICRO_LLM_LAYERS: usize = 2;
pub const MICRO_LLM_MAGIC: &[u8; 8] = b"GENESLLM";

#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct TransformerBlockWeights {
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
}

impl TransformerBlockWeights {
    pub const fn empty() -> Self {
        Self {
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
        }
    }

    pub fn copy_into_smp_ctx(&self, ctx: &mut SmpTransformerContext) {
        ctx.wq.copy_from_slice(&self.wq);
        ctx.bq.copy_from_slice(&self.bq);
        ctx.wk.copy_from_slice(&self.wk);
        ctx.bk.copy_from_slice(&self.bk);
        ctx.wv.copy_from_slice(&self.wv);
        ctx.bv.copy_from_slice(&self.bv);
        ctx.w_ff1.copy_from_slice(&self.w_ff1);
        ctx.b_ff1.copy_from_slice(&self.b_ff1);
        ctx.w_ff2.copy_from_slice(&self.w_ff2);
        ctx.b_ff2.copy_from_slice(&self.b_ff2);
    }
}

#[repr(C, align(64))]
pub struct MicroLlmWeights {
    pub token_emb: [i8; MICRO_LLM_VOCAB * 64],
    pub pos_emb: [i8; 16 * 64],
    pub block0: TransformerBlockWeights,
    pub block1: TransformerBlockWeights,
    pub w_lm: [i8; MICRO_LLM_VOCAB * 64],
    pub b_lm: [i32; MICRO_LLM_VOCAB],
}

impl MicroLlmWeights {
    pub const fn empty() -> Self {
        Self {
            token_emb: [0; MICRO_LLM_VOCAB * 64],
            pos_emb: [0; 16 * 64],
            block0: TransformerBlockWeights::empty(),
            block1: TransformerBlockWeights::empty(),
            w_lm: [0; MICRO_LLM_VOCAB * 64],
            b_lm: [0; MICRO_LLM_VOCAB],
        }
    }
}

pub static mut SMP_MICRO_LLM_WEIGHTS: MicroLlmWeights = MicroLlmWeights::empty();

/// Parses and loads model parameters from contiguous binary file slice.
pub fn load_micro_llm_from_bytes(
    data: &[u8],
    weights: &mut MicroLlmWeights,
) -> Result<(), &'static str> {
    if data.len() < 78368 {
        return Err("Micro-LLM binary too small");
    }
    if &data[0..8] != MICRO_LLM_MAGIC {
        return Err("Invalid Micro-LLM magic");
    }
    let vocab = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
    let d_model = u32::from_le_bytes(data[12..16].try_into().unwrap()) as usize;
    let n_layers = u32::from_le_bytes(data[16..20].try_into().unwrap()) as usize;
    let seq_len = u32::from_le_bytes(data[20..24].try_into().unwrap()) as usize;

    if vocab != MICRO_LLM_VOCAB
        || d_model != MICRO_LLM_D_MODEL
        || n_layers != MICRO_LLM_LAYERS
        || seq_len != MICRO_LLM_SEQ_LEN
    {
        return Err("Micro-LLM dimension mismatch");
    }

    let mut off = 32;

    // token_emb (128 * 64 bytes)
    let token_bytes = MICRO_LLM_VOCAB * 64;
    unsafe {
        core::ptr::copy_nonoverlapping(
            data[off..].as_ptr() as *const i8,
            weights.token_emb.as_mut_ptr(),
            token_bytes,
        );
    }
    off += token_bytes;

    // pos_emb (16 * 64 bytes)
    let pos_bytes = 16 * 64;
    unsafe {
        core::ptr::copy_nonoverlapping(
            data[off..].as_ptr() as *const i8,
            weights.pos_emb.as_mut_ptr(),
            pos_bytes,
        );
    }
    off += pos_bytes;

    // Helper to unpack a transformer block
    unsafe fn unpack_block(slice: &[u8], blk: &mut TransformerBlockWeights) -> usize {
        let mut b_off = 0;
        core::ptr::copy_nonoverlapping(slice[b_off..].as_ptr() as *const i8, blk.wq.as_mut_ptr(), 4096);
        b_off += 4096;
        core::ptr::copy_nonoverlapping(slice[b_off..].as_ptr() as *const i32, blk.bq.as_mut_ptr(), 64);
        b_off += 256;
        core::ptr::copy_nonoverlapping(slice[b_off..].as_ptr() as *const i8, blk.wk.as_mut_ptr(), 4096);
        b_off += 4096;
        core::ptr::copy_nonoverlapping(slice[b_off..].as_ptr() as *const i32, blk.bk.as_mut_ptr(), 64);
        b_off += 256;
        core::ptr::copy_nonoverlapping(slice[b_off..].as_ptr() as *const i8, blk.wv.as_mut_ptr(), 4096);
        b_off += 4096;
        core::ptr::copy_nonoverlapping(slice[b_off..].as_ptr() as *const i32, blk.bv.as_mut_ptr(), 64);
        b_off += 256;
        core::ptr::copy_nonoverlapping(slice[b_off..].as_ptr() as *const i8, blk.w_ff1.as_mut_ptr(), 8192);
        b_off += 8192;
        core::ptr::copy_nonoverlapping(slice[b_off..].as_ptr() as *const i32, blk.b_ff1.as_mut_ptr(), 128);
        b_off += 512;
        core::ptr::copy_nonoverlapping(slice[b_off..].as_ptr() as *const i8, blk.w_ff2.as_mut_ptr(), 8192);
        b_off += 8192;
        core::ptr::copy_nonoverlapping(slice[b_off..].as_ptr() as *const i32, blk.b_ff2.as_mut_ptr(), 64);
        b_off += 256;
        b_off
    }

    unsafe {
        off += unpack_block(&data[off..], &mut weights.block0);
        off += unpack_block(&data[off..], &mut weights.block1);
    }

    // w_lm (128 * 64 bytes)
    let lm_bytes = MICRO_LLM_VOCAB * 64;
    unsafe {
        core::ptr::copy_nonoverlapping(
            data[off..].as_ptr() as *const i8,
            weights.w_lm.as_mut_ptr(),
            lm_bytes,
        );
    }
    off += lm_bytes;

    // b_lm (128 * 4 bytes)
    unsafe {
        core::ptr::copy_nonoverlapping(
            data[off..].as_ptr() as *const i32,
            weights.b_lm.as_mut_ptr(),
            MICRO_LLM_VOCAB,
        );
    }

    Ok(())
}

/// Runs a single autoregressive token generation step using 4-core parallel SMP NEON kernels.
pub unsafe fn run_parallel_micro_llm_step(
    tokens: &[u8],
    weights: &MicroLlmWeights,
    smp_ctx: &mut SmpTransformerContext,
) -> (u8, [i32; MICRO_LLM_VOCAB]) {
    let n = tokens.len().min(16);

    // 1. Embeddings: token_emb + pos_emb
    for t in 0..n {
        let tok = (tokens[t] as usize) % MICRO_LLM_VOCAB;
        for d in 0..64 {
            let te = weights.token_emb[tok * 64 + d] as i32;
            let pe = weights.pos_emb[t * 64 + d] as i32;
            smp_ctx.input[t * 64 + d] = (te + pe).clamp(-128, 127) as i8;
        }
    }
    for t in n..16 {
        for d in 0..64 {
            smp_ctx.input[t * 64 + d] = 0;
        }
    }

    // 2. Block 0 (4-core parallel)
    weights.block0.copy_into_smp_ctx(smp_ctx);
    let out_blk0 = run_parallel_transformer(smp_ctx);

    // 3. Block 1 (4-core parallel)
    smp_ctx.input.copy_from_slice(&out_blk0);
    weights.block1.copy_into_smp_ctx(smp_ctx);
    let out_blk1 = run_parallel_transformer(smp_ctx);

    // 4. LM Head projection on final sequence token
    let last_t = if n > 0 { n - 1 } else { 0 };
    let h = unsafe { &*(&out_blk1[last_t * 64] as *const i8 as *const [i8; 64]) };

    let mut logits = [0i32; MICRO_LLM_VOCAB];
    let mut best_tok = 0usize;
    let mut max_logit = i32::MIN;

    for v in 0..MICRO_LLM_VOCAB {
        let w_row = unsafe { &*(&weights.w_lm[v * 64] as *const i8 as *const [i8; 64]) };
        let logit = neon_dot_product_64(h, w_row, weights.b_lm[v]);
        logits[v] = logit;
        if logit > max_logit {
            max_logit = logit;
            best_tok = v;
        }
    }

    (best_tok as u8, logits)
}

/// Scalar reference implementation for single-step token prediction.
pub fn scalar_micro_llm_step(
    tokens: &[u8],
    weights: &MicroLlmWeights,
) -> (u8, [i32; MICRO_LLM_VOCAB]) {
    let n = tokens.len().min(16);
    let mut input = [0i8; 16 * 64];

    for t in 0..n {
        let tok = (tokens[t] as usize) % MICRO_LLM_VOCAB;
        for d in 0..64 {
            let te = weights.token_emb[tok * 64 + d] as i32;
            let pe = weights.pos_emb[t * 64 + d] as i32;
            input[t * 64 + d] = (te + pe).clamp(-128, 127) as i8;
        }
    }

    // Scalar Block 0
    let out_blk0 = scalar_transformer_block(
        &input,
        &weights.block0.wq,
        &weights.block0.bq,
        &weights.block0.wk,
        &weights.block0.bk,
        &weights.block0.wv,
        &weights.block0.bv,
        &weights.block0.w_ff1,
        &weights.block0.b_ff1,
        &weights.block0.w_ff2,
        &weights.block0.b_ff2,
    );

    // Scalar Block 1
    let out_blk1 = scalar_transformer_block(
        &out_blk0,
        &weights.block1.wq,
        &weights.block1.bq,
        &weights.block1.wk,
        &weights.block1.bk,
        &weights.block1.wv,
        &weights.block1.bv,
        &weights.block1.w_ff1,
        &weights.block1.b_ff1,
        &weights.block1.w_ff2,
        &weights.block1.b_ff2,
    );

    let last_t = if n > 0 { n - 1 } else { 0 };
    let h = unsafe { &*(&out_blk1[last_t * 64] as *const i8 as *const [i8; 64]) };

    let mut logits = [0i32; MICRO_LLM_VOCAB];
    let mut best_tok = 0usize;
    let mut max_logit = i32::MIN;

    for v in 0..MICRO_LLM_VOCAB {
        let w_row = unsafe { &*(&weights.w_lm[v * 64] as *const i8 as *const [i8; 64]) };
        let logit = scalar_dot_product_64(h, w_row, weights.b_lm[v]);
        logits[v] = logit;
        if logit > max_logit {
            max_logit = logit;
            best_tok = v;
        }
    }

    (best_tok as u8, logits)
}

/// Verifies exact equality between parallel NEON generation and scalar reference.
pub fn verify_llm_step(
    parallel_tok: u8,
    parallel_logits: &[i32; MICRO_LLM_VOCAB],
    scalar_tok: u8,
    scalar_logits: &[i32; MICRO_LLM_VOCAB],
) -> bool {
    if parallel_tok != scalar_tok {
        return false;
    }
    for i in 0..MICRO_LLM_VOCAB {
        if parallel_logits[i] != scalar_logits[i] {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn micro_llm_step_parallel_vs_scalar_equivalence() {
        let mut weights = MicroLlmWeights::empty();

        // Populate deterministic non-trivial weights
        for i in 0..weights.token_emb.len() {
            weights.token_emb[i] = (((i * 7) % 19) as i8) - 9;
        }
        for i in 0..weights.pos_emb.len() {
            weights.pos_emb[i] = (((i * 11) % 17) as i8) - 8;
        }
        for i in 0..weights.block0.wq.len() {
            weights.block0.wq[i] = (((i * 3) % 13) as i8) - 6;
            weights.block1.wq[i] = (((i * 5) % 13) as i8) - 6;
        }
        for i in 0..weights.block0.wk.len() {
            weights.block0.wk[i] = (((i * 5) % 11) as i8) - 5;
            weights.block1.wk[i] = (((i * 7) % 11) as i8) - 5;
        }
        for i in 0..weights.block0.wv.len() {
            weights.block0.wv[i] = (((i * 2) % 17) as i8) - 8;
            weights.block1.wv[i] = (((i * 3) % 17) as i8) - 8;
        }
        for i in 0..weights.block0.w_ff1.len() {
            weights.block0.w_ff1[i] = (((i * 7) % 15) as i8) - 7;
            weights.block1.w_ff1[i] = (((i * 9) % 15) as i8) - 7;
        }
        for i in 0..weights.block0.w_ff2.len() {
            weights.block0.w_ff2[i] = (((i * 3) % 11) as i8) - 5;
            weights.block1.w_ff2[i] = (((i * 5) % 11) as i8) - 5;
        }
        for i in 0..weights.w_lm.len() {
            weights.w_lm[i] = (((i * 13) % 15) as i8) - 7;
        }
        weights.b_lm[65] = 500; // bias towards 'A'

        let prompt = b"GENESIS ";
        let (scalar_tok, scalar_logits) = scalar_micro_llm_step(prompt, &weights);

        let mut smp_ctx = SmpTransformerContext {
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

        let (parallel_tok, parallel_logits) = unsafe {
            run_parallel_micro_llm_step(prompt, &weights, &mut smp_ctx)
        };

        assert!(
            verify_llm_step(parallel_tok, &parallel_logits, scalar_tok, &scalar_logits),
            "Micro-LLM parallel NEON execution must match scalar reference bit-for-bit"
        );
    }
}
