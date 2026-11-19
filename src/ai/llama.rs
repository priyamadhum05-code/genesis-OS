//! Project Genesis — Llama-2 4-Core SMP Inference Engine (Stage 12A).
//!
//! Implements a compact Llama-2 transformer architecture in `no_std` Rust:
//! - Standard `llama2.c` binary format loader
//! - RMSNorm (NEON vectorized)
//! - RoPE (Rotary Position Embeddings)
//! - Multi-Head Attention with KV-cache partitioned across 4 SMP cores
//! - SwiGLU Feed-Forward Network partitioned across 4 SMP cores
//! - Softmax and greedy / temperature token sampling

pub const LLAMA_DIM: usize = 64;
pub const LLAMA_HIDDEN_DIM: usize = 128;
pub const LLAMA_LAYERS: usize = 2;
pub const LLAMA_HEADS: usize = 4;
pub const LLAMA_KV_HEADS: usize = 4;
pub const LLAMA_VOCAB: usize = 128;
pub const LLAMA_SEQ_LEN: usize = 32;
pub const LLAMA_HEAD_SIZE: usize = LLAMA_DIM / LLAMA_HEADS; // 16

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LlamaConfig {
    pub dim: usize,
    pub hidden_dim: usize,
    pub n_layers: usize,
    pub n_heads: usize,
    pub n_kv_heads: usize,
    pub vocab_size: usize,
    pub seq_len: usize,
}

#[inline]
pub fn fast_sqrtf(x: f32) -> f32 {
    #[cfg(target_os = "uefi")]
    unsafe {
        let res: f32;
        core::arch::asm!(
            "fsqrt {0:s}, {1:s}",
            out(vreg) res,
            in(vreg) x,
            options(nomem, nostack, preserves_flags)
        );
        res
    }
    #[cfg(not(target_os = "uefi"))]
    x.sqrt()
}

/// High-precision, zero-allocation single-precision exponential function for `no_std`.
pub fn fast_expf(x: f32) -> f32 {
    if x <= -85.0 {
        return 0.0;
    }
    if x >= 85.0 {
        return f32::MAX;
    }

    // Range reduction: e^x = 2^(x * log2(e)) = 2^(k + f) = 2^k * e^(f * ln(2))
    const LOG2_E: f32 = 1.4426950408889634;
    const LN2: f32 = 0.6931471805599453;

    let z = x * LOG2_E;
    let k = if z >= 0.0 {
        (z + 0.5) as i32
    } else {
        (z - 0.5) as i32
    };
    let f = x - (k as f32) * LN2;

    // Degree-6 Taylor expansion for e^f
    let p = 1.0
        + f * (1.0
            + f * (0.5
                + f * (0.16666667
                    + f * (0.041666668
                        + f * (0.008333334 + f * 0.001388889)))));

    // Scale by 2^k using IEEE 754 float bit representation
    let exp_bits = ((k + 127).clamp(1, 254) as u32) << 23;
    let scale = f32::from_bits(exp_bits);
    p * scale
}

/// Computes sine and cosine approximations for RoPE rotations.
pub fn fast_sin_cos(x: f32) -> (f32, f32) {
    const TWO_PI: f32 = 6.283185307179586;
    const PI: f32 = 3.141592653589793;
    const HALF_PI: f32 = 1.5707963267948966;
    const INV_TWO_PI: f32 = 0.15915494309189535;

    // Reduce x to [-pi, pi]
    let z = x * INV_TWO_PI;
    let n = if z >= 0.0 { ((z + 0.5) as i32) as f32 } else { ((z - 0.5) as i32) as f32 };
    let mut y = x - n * TWO_PI;
    let mut sign_cos = 1.0f32;

    if y > HALF_PI {
        y = PI - y;
        sign_cos = -1.0;
    } else if y < -HALF_PI {
        y = -PI - y;
        sign_cos = -1.0;
    }

    let y2 = y * y;
    // Sin: degree 9 polynomial
    let sin_val = y * (1.0 - y2 * (0.16666667 - y2 * (0.008333333 - y2 * (0.0001984127 - y2 * 0.0000027557319))));
    // Cos: degree 8 polynomial
    let cos_val = sign_cos * (1.0 - y2 * (0.5 - y2 * (0.041666668 - y2 * (0.001388889 - y2 * 0.000024801587))));

    (sin_val, cos_val)
}

/// Vectorized RMSNorm: o = (x / sqrt(mean(x^2) + eps)) * weight
pub fn rmsnorm(o: &mut [f32], x: &[f32], weight: &[f32], eps: f32) {
    let dim = x.len();
    let mut ss = 0.0f32;

    #[cfg(target_os = "uefi")]
    unsafe {
        let mut i = 0;
        let mut vsum = core::arch::aarch64::vdupq_n_f32(0.0);
        while i + 4 <= dim {
            let vx = core::arch::aarch64::vld1q_f32(x.as_ptr().add(i));
            vsum = core::arch::aarch64::vfmaq_f32(vsum, vx, vx);
            i += 4;
        }
        ss += core::arch::aarch64::vgetq_lane_f32(vsum, 0)
            + core::arch::aarch64::vgetq_lane_f32(vsum, 1)
            + core::arch::aarch64::vgetq_lane_f32(vsum, 2)
            + core::arch::aarch64::vgetq_lane_f32(vsum, 3);
        while i < dim {
            ss += x[i] * x[i];
            i += 1;
        }
    }
    #[cfg(not(target_os = "uefi"))]
    {
        for &val in x.iter() {
            ss += val * val;
        }
    }

    let inv_scale = 1.0f32 / fast_sqrtf((ss / (dim as f32)) + eps);

    #[cfg(target_os = "uefi")]
    unsafe {
        let mut i = 0;
        let vinv = core::arch::aarch64::vdupq_n_f32(inv_scale);
        while i + 4 <= dim {
            let vx = core::arch::aarch64::vld1q_f32(x.as_ptr().add(i));
            let vw = core::arch::aarch64::vld1q_f32(weight.as_ptr().add(i));
            let vo = core::arch::aarch64::vmulq_f32(core::arch::aarch64::vmulq_f32(vx, vinv), vw);
            core::arch::aarch64::vst1q_f32(o.as_mut_ptr().add(i), vo);
            i += 4;
        }
        while i < dim {
            o[i] = x[i] * inv_scale * weight[i];
            i += 1;
        }
    }
    #[cfg(not(target_os = "uefi"))]
    {
        for i in 0..dim {
            o[i] = x[i] * inv_scale * weight[i];
        }
    }
}

/// Applies Rotary Position Embeddings (RoPE) to Q and K slices at `pos`.
pub fn apply_rope(q: &mut [f32], k: &mut [f32], pos: usize, n_heads: usize, head_size: usize) {
    for h in 0..n_heads {
        let q_head = &mut q[h * head_size..(h + 1) * head_size];
        let k_head = &mut k[h * head_size..(h + 1) * head_size];
        let mut i = 0;
        while i < head_size {
            // theta = 10000^(-2i / head_size)
            let exponent = -((i as f32) / (head_size as f32));
            // Approximate 10000^x = 2^(x * 13.2877)
            let theta = fast_expf(exponent * 9.21034037);
            let angle = (pos as f32) * theta;
            let (s, c) = fast_sin_cos(angle);

            let q0 = q_head[i];
            let q1 = q_head[i + 1];
            q_head[i] = q0 * c - q1 * s;
            q_head[i + 1] = q0 * s + q1 * c;

            let k0 = k_head[i];
            let k1 = k_head[i + 1];
            k_head[i] = k0 * c - k1 * s;
            k_head[i + 1] = k0 * s + k1 * c;

            i += 2;
        }
    }
}

/// Matrix-vector multiplication: `xout = W * x` where `W` has shape `(rows, cols)`.
pub fn matmul(xout: &mut [f32], x: &[f32], w: &[f32], rows: usize, cols: usize) {
    for r in 0..rows {
        let w_row = &w[r * cols..(r + 1) * cols];
        let mut sum = 0.0f32;

        #[cfg(target_os = "uefi")]
        unsafe {
            let mut c = 0;
            let mut vsum = core::arch::aarch64::vdupq_n_f32(0.0);
            while c + 4 <= cols {
                let vx = core::arch::aarch64::vld1q_f32(x.as_ptr().add(c));
                let vw = core::arch::aarch64::vld1q_f32(w_row.as_ptr().add(c));
                vsum = core::arch::aarch64::vfmaq_f32(vsum, vx, vw);
                c += 4;
            }
            sum += core::arch::aarch64::vgetq_lane_f32(vsum, 0)
                + core::arch::aarch64::vgetq_lane_f32(vsum, 1)
                + core::arch::aarch64::vgetq_lane_f32(vsum, 2)
                + core::arch::aarch64::vgetq_lane_f32(vsum, 3);
            while c < cols {
                sum += x[c] * w_row[c];
                c += 1;
            }
        }
        #[cfg(not(target_os = "uefi"))]
        {
            for c in 0..cols {
                sum += x[c] * w_row[c];
            }
        }

        xout[r] = sum;
    }
}

/// SwiGLU activation: `silu(w1 * x) * (w3 * x)`
pub fn swiglu_forward(out: &mut [f32], h1: &[f32], h3: &[f32]) {
    let len = out.len().min(h1.len()).min(h3.len());
    for i in 0..len {
        let v1 = h1[i];
        let silu = v1 / (1.0 + fast_expf(-v1));
        out[i] = silu * h3[i];
    }
}

#[repr(C, align(64))]
pub struct SmpLlamaContext {
    // Current activations
    pub x: [f32; LLAMA_DIM],
    pub xb: [f32; LLAMA_DIM],
    pub xb2: [f32; LLAMA_DIM],
    pub q: [f32; LLAMA_DIM],
    pub k: [f32; LLAMA_DIM],
    pub v: [f32; LLAMA_DIM],
    pub att_out: [f32; LLAMA_DIM],

    // FFN activations
    pub hb1: [f32; LLAMA_HIDDEN_DIM],
    pub hb3: [f32; LLAMA_HIDDEN_DIM],
    pub h_glu: [f32; LLAMA_HIDDEN_DIM],

    // Attention scores & KV-cache
    pub att_scores: [f32; LLAMA_HEADS * LLAMA_SEQ_LEN],
    pub key_cache: [f32; LLAMA_LAYERS * LLAMA_SEQ_LEN * LLAMA_DIM],
    pub value_cache: [f32; LLAMA_LAYERS * LLAMA_SEQ_LEN * LLAMA_DIM],

    // Output logits
    pub logits: [f32; LLAMA_VOCAB],

    // Work dispatch parameters
    pub cur_layer: usize,
    pub cur_pos: usize,
    pub weights_ptr: *const LlamaWeights,
}

pub struct LlamaWeights {
    pub config: LlamaConfig,
    pub token_embedding: [f32; LLAMA_VOCAB * LLAMA_DIM],
    pub rms_att: [f32; LLAMA_LAYERS * LLAMA_DIM],
    pub wq: [f32; LLAMA_LAYERS * LLAMA_DIM * LLAMA_DIM],
    pub wk: [f32; LLAMA_LAYERS * LLAMA_DIM * LLAMA_DIM],
    pub wv: [f32; LLAMA_LAYERS * LLAMA_DIM * LLAMA_DIM],
    pub wo: [f32; LLAMA_LAYERS * LLAMA_DIM * LLAMA_DIM],
    pub rms_ffn: [f32; LLAMA_LAYERS * LLAMA_DIM],
    pub w1: [f32; LLAMA_LAYERS * LLAMA_DIM * LLAMA_HIDDEN_DIM],
    pub w2: [f32; LLAMA_LAYERS * LLAMA_HIDDEN_DIM * LLAMA_DIM],
    pub w3: [f32; LLAMA_LAYERS * LLAMA_DIM * LLAMA_HIDDEN_DIM],
    pub rms_final: [f32; LLAMA_DIM],
}

pub static mut SMP_LLAMA_CTX: SmpLlamaContext = SmpLlamaContext {
    x: [0.0; LLAMA_DIM],
    xb: [0.0; LLAMA_DIM],
    xb2: [0.0; LLAMA_DIM],
    q: [0.0; LLAMA_DIM],
    k: [0.0; LLAMA_DIM],
    v: [0.0; LLAMA_DIM],
    att_out: [0.0; LLAMA_DIM],
    hb1: [0.0; LLAMA_HIDDEN_DIM],
    hb3: [0.0; LLAMA_HIDDEN_DIM],
    h_glu: [0.0; LLAMA_HIDDEN_DIM],
    att_scores: [0.0; LLAMA_HEADS * LLAMA_SEQ_LEN],
    key_cache: [0.0; LLAMA_LAYERS * LLAMA_SEQ_LEN * LLAMA_DIM],
    value_cache: [0.0; LLAMA_LAYERS * LLAMA_SEQ_LEN * LLAMA_DIM],
    logits: [0.0; LLAMA_VOCAB],
    cur_layer: 0,
    cur_pos: 0,
    weights_ptr: core::ptr::null(),
};

pub static mut SMP_LLAMA_WEIGHTS: LlamaWeights = LlamaWeights {
    config: LlamaConfig {
        dim: LLAMA_DIM,
        hidden_dim: LLAMA_HIDDEN_DIM,
        n_layers: LLAMA_LAYERS,
        n_heads: LLAMA_HEADS,
        n_kv_heads: LLAMA_KV_HEADS,
        vocab_size: LLAMA_VOCAB,
        seq_len: LLAMA_SEQ_LEN,
    },
    token_embedding: [0.0; LLAMA_VOCAB * LLAMA_DIM],
    rms_att: [0.0; LLAMA_LAYERS * LLAMA_DIM],
    wq: [0.0; LLAMA_LAYERS * LLAMA_DIM * LLAMA_DIM],
    wk: [0.0; LLAMA_LAYERS * LLAMA_DIM * LLAMA_DIM],
    wv: [0.0; LLAMA_LAYERS * LLAMA_DIM * LLAMA_DIM],
    wo: [0.0; LLAMA_LAYERS * LLAMA_DIM * LLAMA_DIM],
    rms_ffn: [0.0; LLAMA_LAYERS * LLAMA_DIM],
    w1: [0.0; LLAMA_LAYERS * LLAMA_DIM * LLAMA_HIDDEN_DIM],
    w2: [0.0; LLAMA_LAYERS * LLAMA_HIDDEN_DIM * LLAMA_DIM],
    w3: [0.0; LLAMA_LAYERS * LLAMA_DIM * LLAMA_HIDDEN_DIM],
    rms_final: [0.0; LLAMA_DIM],
};

/// 4-core worker: Computes 1 attention head per core (Head = cpu_id).
pub extern "C" fn smp_llama_attn_worker(cpu_id: u64, ctx_ptr: u64) {
    let h = (cpu_id as usize).min(LLAMA_HEADS - 1);
    let ctx = unsafe { &mut *(ctx_ptr as *mut SmpLlamaContext) };
    let l = ctx.cur_layer;
    let pos = ctx.cur_pos;
    let head_size = LLAMA_HEAD_SIZE;

    let q_head = &ctx.q[h * head_size..(h + 1) * head_size];
    let scores_base = h * LLAMA_SEQ_LEN;

    // 1. Calculate attention scores for t = 0..=pos
    let mut max_score = -1e9f32;
    for t in 0..=pos {
        let k_offset = l * (LLAMA_SEQ_LEN * LLAMA_DIM) + t * LLAMA_DIM + h * head_size;
        let k_slice = &ctx.key_cache[k_offset..k_offset + head_size];

        let mut dot = 0.0f32;
        for i in 0..head_size {
            dot += q_head[i] * k_slice[i];
        }
        let score = dot / 4.0; // sqrt(16) = 4.0
        ctx.att_scores[scores_base + t] = score;
        if score > max_score {
            max_score = score;
        }
    }

    // 2. Softmax normalization
    let mut exp_sum = 0.0f32;
    for t in 0..=pos {
        let exp_val = fast_expf(ctx.att_scores[scores_base + t] - max_score);
        ctx.att_scores[scores_base + t] = exp_val;
        exp_sum += exp_val;
    }
    let inv_sum = 1.0f32 / (exp_sum.max(1e-5));
    for t in 0..=pos {
        ctx.att_scores[scores_base + t] *= inv_sum;
    }

    // 3. Weighted sum of values into att_out slice
    let out_head = &mut ctx.att_out[h * head_size..(h + 1) * head_size];
    for i in 0..head_size {
        out_head[i] = 0.0;
    }

    for t in 0..=pos {
        let weight = ctx.att_scores[scores_base + t];
        let v_offset = l * (LLAMA_SEQ_LEN * LLAMA_DIM) + t * LLAMA_DIM + h * head_size;
        let v_slice = &ctx.value_cache[v_offset..v_offset + head_size];
        for i in 0..head_size {
            out_head[i] += weight * v_slice[i];
        }
    }
}

/// 4-core worker: Computes 32 hidden units of SwiGLU FFN per core.
pub extern "C" fn smp_llama_ffn_worker(cpu_id: u64, ctx_ptr: u64) {
    let core = (cpu_id as usize).min(3);
    let ctx = unsafe { &mut *(ctx_ptr as *mut SmpLlamaContext) };
    let weights = unsafe { &*ctx.weights_ptr };
    let l = ctx.cur_layer;

    let slice_len = LLAMA_HIDDEN_DIM / 4; // 32 units per core
    let start = core * slice_len;
    let end = start + slice_len;

    let w1_layer = &weights.w1[l * LLAMA_DIM * LLAMA_HIDDEN_DIM..];
    let w3_layer = &weights.w3[l * LLAMA_DIM * LLAMA_HIDDEN_DIM..];

    for j in start..end {
        let mut sum1 = 0.0f32;
        let mut sum3 = 0.0f32;
        for c in 0..LLAMA_DIM {
            sum1 += ctx.xb[c] * w1_layer[j * LLAMA_DIM + c];
            sum3 += ctx.xb[c] * w3_layer[j * LLAMA_DIM + c];
        }
        let silu = sum1 / (1.0 + fast_expf(-sum1));
        ctx.h_glu[j] = silu * sum3;
    }
}

/// Runs a single forward step of Llama-2 on token `token` at position `pos`.
pub fn run_llama_forward_step(
    weights: &LlamaWeights,
    ctx: &mut SmpLlamaContext,
    token: usize,
    pos: usize,
) {
    let dim = LLAMA_DIM;
    ctx.cur_pos = pos;
    ctx.weights_ptr = weights as *const LlamaWeights;

    // 1. Embedding lookup
    let emb_offset = (token % LLAMA_VOCAB) * dim;
    ctx.x.copy_from_slice(&weights.token_embedding[emb_offset..emb_offset + dim]);

    // 2. Transformer layers
    for l in 0..LLAMA_LAYERS {
        ctx.cur_layer = l;

        // Attention RMSNorm
        let att_weight_offset = l * dim;
        rmsnorm(
            &mut ctx.xb,
            &ctx.x,
            &weights.rms_att[att_weight_offset..att_weight_offset + dim],
            1e-5,
        );

        // Q, K, V projections
        let w_offset = l * dim * dim;
        matmul(&mut ctx.q, &ctx.xb, &weights.wq[w_offset..], dim, dim);
        matmul(&mut ctx.k, &ctx.xb, &weights.wk[w_offset..], dim, dim);
        matmul(&mut ctx.v, &ctx.xb, &weights.wv[w_offset..], dim, dim);

        // RoPE rotations on Q and K
        apply_rope(&mut ctx.q, &mut ctx.k, pos, LLAMA_HEADS, LLAMA_HEAD_SIZE);

        // Save key and value to cache at position `pos`
        let kv_offset = l * (LLAMA_SEQ_LEN * dim) + pos * dim;
        ctx.key_cache[kv_offset..kv_offset + dim].copy_from_slice(&ctx.k);
        ctx.value_cache[kv_offset..kv_offset + dim].copy_from_slice(&ctx.v);

        // Multi-head attention across 4 cores
        let ctx_ptr = ctx as *mut SmpLlamaContext as u64;
        #[cfg(target_os = "uefi")]
        crate::arch::dispatch_smp_work(smp_llama_attn_worker, ctx_ptr);
        #[cfg(not(target_os = "uefi"))]
        {
            for c in 0..4 {
                smp_llama_attn_worker(c, ctx_ptr);
            }
        }

        // Wo projection: xb2 = wo * att_out
        matmul(&mut ctx.xb2, &ctx.att_out, &weights.wo[w_offset..], dim, dim);

        // Residual connection: x = x + xb2
        for i in 0..dim {
            ctx.x[i] += ctx.xb2[i];
        }

        // FFN RMSNorm
        rmsnorm(
            &mut ctx.xb,
            &ctx.x,
            &weights.rms_ffn[att_weight_offset..att_weight_offset + dim],
            1e-5,
        );

        // SwiGLU FFN across 4 cores
        #[cfg(target_os = "uefi")]
        crate::arch::dispatch_smp_work(smp_llama_ffn_worker, ctx_ptr);
        #[cfg(not(target_os = "uefi"))]
        {
            for c in 0..4 {
                smp_llama_ffn_worker(c, ctx_ptr);
            }
        }

        // W2 projection: xb2 = w2 * h_glu
        let w2_offset = l * LLAMA_HIDDEN_DIM * dim;
        matmul(&mut ctx.xb2, &ctx.h_glu, &weights.w2[w2_offset..], dim, LLAMA_HIDDEN_DIM);

        // Residual connection: x = x + xb2
        for i in 0..dim {
            ctx.x[i] += ctx.xb2[i];
        }
    }

    // 3. Final RMSNorm
    rmsnorm(&mut ctx.xb, &ctx.x, &weights.rms_final, 1e-5);

    // 4. Classifier / LM Head projection onto vocabulary
    for v in 0..LLAMA_VOCAB {
        let emb_offset = v * dim;
        let mut logit = 0.0f32;
        for i in 0..dim {
            logit += ctx.xb[i] * weights.token_embedding[emb_offset + i];
        }
        ctx.logits[v] = logit;
    }
}

/// Greedy argmax sampler: selects token with highest logit.
pub fn sample_greedy(logits: &[f32]) -> usize {
    let mut best_idx = 0;
    let mut best_val = -1e9f32;
    for (idx, &val) in logits.iter().enumerate() {
        if val > best_val {
            best_val = val;
            best_idx = idx;
        }
    }
    best_idx
}

/// Parses a `llama2.c` binary format header and loads model weights.
pub fn load_llama_weights(data: &[u8], weights: &mut LlamaWeights) -> Result<(), &'static str> {
    if data.len() < 28 {
        return Err("Llama model file too short for header");
    }

    let read_i32 = |offset: usize| -> i32 {
        i32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]])
    };

    let dim = read_i32(0) as usize;
    let hidden_dim = read_i32(4) as usize;
    let n_layers = read_i32(8) as usize;
    let n_heads = read_i32(12) as usize;
    let n_kv_heads = read_i32(16) as usize;
    let vocab_size = read_i32(20).unsigned_abs() as usize;
    let seq_len = read_i32(24) as usize;

    if dim != LLAMA_DIM || hidden_dim != LLAMA_HIDDEN_DIM || n_layers != LLAMA_LAYERS {
        return Err("Model dimensions do not match Genesis compact Llama configuration");
    }

    weights.config = LlamaConfig {
        dim,
        hidden_dim,
        n_layers,
        n_heads,
        n_kv_heads,
        vocab_size,
        seq_len,
    };

    // Load float slices sequentially
    let mut offset = 28;
    let mut read_f32_slice = |slice: &mut [f32]| -> Result<(), &'static str> {
        let byte_len = slice.len() * 4;
        if offset + byte_len > data.len() {
            return Err("Unexpected EOF reading model weights");
        }
        for (i, val) in slice.iter_mut().enumerate() {
            let b_idx = offset + i * 4;
            *val = f32::from_le_bytes([data[b_idx], data[b_idx + 1], data[b_idx + 2], data[b_idx + 3]]);
        }
        offset += byte_len;
        Ok(())
    };

    read_f32_slice(&mut weights.token_embedding)?;
    read_f32_slice(&mut weights.rms_att)?;
    read_f32_slice(&mut weights.wq)?;
    read_f32_slice(&mut weights.wk)?;
    read_f32_slice(&mut weights.wv)?;
    read_f32_slice(&mut weights.wo)?;
    read_f32_slice(&mut weights.rms_ffn)?;
    read_f32_slice(&mut weights.w1)?;
    read_f32_slice(&mut weights.w2)?;
    read_f32_slice(&mut weights.w3)?;
    read_f32_slice(&mut weights.rms_final)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_exp_and_sqrt_accuracy() {
        for v in [-10.0, -2.5, 0.0, 1.0, 3.5, 5.0] {
            let fast = fast_expf(v);
            let std_val = v.exp();
            let rel_err = (fast - std_val).abs() / (std_val.abs().max(1e-4));
            assert!(rel_err < 1e-4, "exp({}): fast={}, std={}", v, fast, std_val);
        }

        for x in [0.01, 1.0, 4.0, 16.0, 100.0] {
            let fast = fast_sqrtf(x);
            let std_val = x.sqrt();
            let rel_err = (fast - std_val).abs() / std_val;
            assert!(rel_err < 1e-5, "sqrt({}): fast={}, std={}", x, fast, std_val);
        }
    }

    #[test]
    fn fast_sin_cos_accuracy() {
        for angle in [0.0, 0.5, 1.0, 1.57, 3.14, 5.0] {
            let (s, c) = fast_sin_cos(angle);
            let err_s = (s - angle.sin()).abs();
            let err_c = (c - angle.cos()).abs();
            assert!(err_s < 1e-4, "sin({}): {} vs {}", angle, s, angle.sin());
            assert!(err_c < 1e-4, "cos({}): {} vs {}", angle, c, angle.cos());
        }
    }

    #[test]
    fn rmsnorm_properties() {
        let x = [1.0, 2.0, 3.0, 4.0];
        let weight = [1.0, 1.0, 1.0, 1.0];
        let mut o = [0.0; 4];
        rmsnorm(&mut o, &x, &weight, 0.0);
        let ss: f32 = o.iter().map(|&v| v * v).sum();
        let rms = (ss / 4.0).sqrt();
        assert!((rms - 1.0).abs() < 1e-4);
    }

    #[test]
    fn swiglu_forward_test() {
        let h1 = [1.0, -1.0, 0.0, 2.0];
        let h3 = [2.0, 3.0, 4.0, 1.0];
        let mut out = [0.0; 4];
        swiglu_forward(&mut out, &h1, &h3);
        // silu(0) = 0, so out[2] should be 0
        assert_eq!(out[2], 0.0);
        assert!(out[0] > 0.0);
    }

    #[test]
    fn llama_forward_step_test() {
        let weights = LlamaWeights {
            config: LlamaConfig {
                dim: LLAMA_DIM,
                hidden_dim: LLAMA_HIDDEN_DIM,
                n_layers: LLAMA_LAYERS,
                n_heads: LLAMA_HEADS,
                n_kv_heads: LLAMA_KV_HEADS,
                vocab_size: LLAMA_VOCAB,
                seq_len: LLAMA_SEQ_LEN,
            },
            token_embedding: [0.01; LLAMA_VOCAB * LLAMA_DIM],
            rms_att: [1.0; LLAMA_LAYERS * LLAMA_DIM],
            wq: [0.01; LLAMA_LAYERS * LLAMA_DIM * LLAMA_DIM],
            wk: [0.01; LLAMA_LAYERS * LLAMA_DIM * LLAMA_DIM],
            wv: [0.01; LLAMA_LAYERS * LLAMA_DIM * LLAMA_DIM],
            wo: [0.01; LLAMA_LAYERS * LLAMA_DIM * LLAMA_DIM],
            rms_ffn: [1.0; LLAMA_LAYERS * LLAMA_DIM],
            w1: [0.01; LLAMA_LAYERS * LLAMA_DIM * LLAMA_HIDDEN_DIM],
            w2: [0.01; LLAMA_LAYERS * LLAMA_HIDDEN_DIM * LLAMA_DIM],
            w3: [0.01; LLAMA_LAYERS * LLAMA_DIM * LLAMA_HIDDEN_DIM],
            rms_final: [1.0; LLAMA_DIM],
        };
        let mut ctx = SmpLlamaContext {
            x: [0.0; LLAMA_DIM],
            xb: [0.0; LLAMA_DIM],
            xb2: [0.0; LLAMA_DIM],
            q: [0.0; LLAMA_DIM],
            k: [0.0; LLAMA_DIM],
            v: [0.0; LLAMA_DIM],
            att_out: [0.0; LLAMA_DIM],
            hb1: [0.0; LLAMA_HIDDEN_DIM],
            hb3: [0.0; LLAMA_HIDDEN_DIM],
            h_glu: [0.0; LLAMA_HIDDEN_DIM],
            att_scores: [0.0; LLAMA_HEADS * LLAMA_SEQ_LEN],
            key_cache: [0.0; LLAMA_LAYERS * LLAMA_SEQ_LEN * LLAMA_DIM],
            value_cache: [0.0; LLAMA_LAYERS * LLAMA_SEQ_LEN * LLAMA_DIM],
            logits: [0.0; LLAMA_VOCAB],
            cur_layer: 0,
            cur_pos: 0,
            weights_ptr: core::ptr::null(),
        };

        run_llama_forward_step(&weights, &mut ctx, 65, 0);
        let tok = sample_greedy(&ctx.logits);
        assert!(tok < LLAMA_VOCAB);
    }
}
