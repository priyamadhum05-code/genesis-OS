//! Project Genesis — Dynamic Hugepage-Backed Llama-2 4-Core SMP Inference Engine (Stage 13A).
//!
//! Implements a compact, dynamically-allocated Llama-2 transformer architecture in `no_std` Rust:
//! - Standard `llama2.c` binary format loader
//! - Dynamically allocated weights and KV-cache backed by KERNEL_PMM + HHDM_BASE
//! - RMSNorm (NEON vectorized)
//! - RoPE (Rotary Position Embeddings)
//! - Dynamic Multi-Head Attention partitioned across 4 SMP cores (`cpu_id * heads_per_core`)
//! - Dynamic SwiGLU Feed-Forward Network partitioned across 4 SMP cores (`cpu_id * slice_len`)
//! - Softmax and greedy token sampling

pub const LLAMA_DIM: usize = 64;
pub const LLAMA_HIDDEN_DIM: usize = 128;
pub const LLAMA_LAYERS: usize = 2;
pub const LLAMA_HEADS: usize = 4;
pub const LLAMA_KV_HEADS: usize = 4;
pub const LLAMA_VOCAB: usize = 128;
pub const LLAMA_SEQ_LEN: usize = 32;
pub const LLAMA_HEAD_SIZE: usize = 16;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct LlamaConfig {
    pub dim: usize,
    pub hidden_dim: usize,
    pub n_layers: usize,
    pub n_heads: usize,
    pub n_kv_heads: usize,
    pub vocab_size: usize,
    pub seq_len: usize,
    pub shared_weights: bool,
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
pub fn apply_rope(
    q: &mut [f32],
    k: &mut [f32],
    pos: usize,
    n_heads: usize,
    n_kv_heads: usize,
    head_size: usize,
) {
    for h in 0..n_heads {
        let q_head = &mut q[h * head_size..(h + 1) * head_size];
        let mut i = 0;
        while i < head_size {
            // theta = 10000^(-2i / head_size)
            let exponent = -((i as f32) / (head_size as f32));
            let theta = fast_expf(exponent * 9.21034037);
            let angle = (pos as f32) * theta;
            let (s, c) = fast_sin_cos(angle);

            let q0 = q_head[i];
            let q1 = q_head[i + 1];
            q_head[i] = q0 * c - q1 * s;
            q_head[i + 1] = q0 * s + q1 * c;

            i += 2;
        }
    }
    for h in 0..n_kv_heads {
        let k_head = &mut k[h * head_size..(h + 1) * head_size];
        let mut i = 0;
        while i < head_size {
            let exponent = -((i as f32) / (head_size as f32));
            let theta = fast_expf(exponent * 9.21034037);
            let angle = (pos as f32) * theta;
            let (s, c) = fast_sin_cos(angle);

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

/// Dynamically allocated model weights backed by PMM/HHDM memory.
pub struct LlamaWeights {
    pub config: LlamaConfig,
    pub token_embedding: *const f32,
    pub rms_att: *const f32,
    pub wq: *const f32,
    pub wk: *const f32,
    pub wv: *const f32,
    pub wo: *const f32,
    pub rms_ffn: *const f32,
    pub w1: *const f32,
    pub w2: *const f32,
    pub w3: *const f32,
    pub rms_final: *const f32,
    pub wcls: *const f32,
    pub total_floats: usize,
    pub paddr: u64,
    pub page_count: usize,
}

unsafe impl Send for LlamaWeights {}
unsafe impl Sync for LlamaWeights {}

impl LlamaWeights {
    pub const fn empty() -> Self {
        Self {
            config: LlamaConfig {
                dim: 0,
                hidden_dim: 0,
                n_layers: 0,
                n_heads: 0,
                n_kv_heads: 0,
                vocab_size: 0,
                seq_len: 0,
                shared_weights: true,
            },
            token_embedding: core::ptr::null(),
            rms_att: core::ptr::null(),
            wq: core::ptr::null(),
            wk: core::ptr::null(),
            wv: core::ptr::null(),
            wo: core::ptr::null(),
            rms_ffn: core::ptr::null(),
            w1: core::ptr::null(),
            w2: core::ptr::null(),
            w3: core::ptr::null(),
            rms_final: core::ptr::null(),
            wcls: core::ptr::null(),
            total_floats: 0,
            paddr: 0,
            page_count: 0,
        }
    }

    pub fn total_floats_for(cfg: &LlamaConfig) -> usize {
        let dim = cfg.dim;
        let hidden = cfg.hidden_dim;
        let layers = cfg.n_layers;
        let vocab = cfg.vocab_size;
        let kv_dim = if cfg.n_heads > 0 { (dim * cfg.n_kv_heads) / cfg.n_heads } else { dim };

        vocab * dim              // token_embedding
        + layers * dim           // rms_att
        + layers * dim * dim     // wq
        + layers * dim * kv_dim  // wk
        + layers * dim * kv_dim  // wv
        + layers * dim * dim     // wo
        + layers * dim           // rms_ffn
        + layers * dim * hidden  // w1
        + layers * hidden * dim  // w2
        + layers * dim * hidden  // w3
        + dim                    // rms_final
        + if cfg.shared_weights { 0 } else { vocab * dim } // wcls
    }

    pub unsafe fn init_from_buffer(&mut self, cfg: LlamaConfig, base_ptr: *mut f32) {
        self.config = cfg;
        let dim = cfg.dim;
        let hidden = cfg.hidden_dim;
        let layers = cfg.n_layers;
        let vocab = cfg.vocab_size;
        let kv_dim = if cfg.n_heads > 0 { (dim * cfg.n_kv_heads) / cfg.n_heads } else { dim };

        let mut p = base_ptr;
        self.token_embedding = p; p = p.add(vocab * dim);
        self.rms_att = p; p = p.add(layers * dim);
        self.wq = p; p = p.add(layers * dim * dim);
        self.wk = p; p = p.add(layers * dim * kv_dim);
        self.wv = p; p = p.add(layers * dim * kv_dim);
        self.wo = p; p = p.add(layers * dim * dim);
        self.rms_ffn = p; p = p.add(layers * dim);
        self.w1 = p; p = p.add(layers * dim * hidden);
        self.w2 = p; p = p.add(layers * hidden * dim);
        self.w3 = p; p = p.add(layers * dim * hidden);
        self.rms_final = p; p = p.add(dim);
        if cfg.shared_weights {
            self.wcls = self.token_embedding;
        } else {
            self.wcls = p; p = p.add(vocab * dim);
        }

        self.total_floats = p.offset_from(base_ptr) as usize;
    }
}

/// Dynamically allocated SMP Llama inference context backed by PMM/HHDM memory.
#[repr(C, align(64))]
pub struct SmpLlamaContext {
    pub config: LlamaConfig,
    pub x: *mut f32,
    pub xb: *mut f32,
    pub xb2: *mut f32,
    pub q: *mut f32,
    pub k: *mut f32,
    pub v: *mut f32,
    pub att_out: *mut f32,

    pub hb1: *mut f32,
    pub hb3: *mut f32,
    pub h_glu: *mut f32,

    pub att_scores: *mut f32,
    pub key_cache: *mut f32,
    pub value_cache: *mut f32,
    pub logits: *mut f32,

    pub cur_layer: usize,
    pub cur_pos: usize,
    pub weights_ptr: *const LlamaWeights,

    pub total_floats: usize,
    pub paddr: u64,
    pub page_count: usize,
}

unsafe impl Send for SmpLlamaContext {}
unsafe impl Sync for SmpLlamaContext {}

impl SmpLlamaContext {
    pub const fn empty() -> Self {
        Self {
            config: LlamaConfig {
                dim: 0,
                hidden_dim: 0,
                n_layers: 0,
                n_heads: 0,
                n_kv_heads: 0,
                vocab_size: 0,
                seq_len: 0,
                shared_weights: true,
            },
            x: core::ptr::null_mut(),
            xb: core::ptr::null_mut(),
            xb2: core::ptr::null_mut(),
            q: core::ptr::null_mut(),
            k: core::ptr::null_mut(),
            v: core::ptr::null_mut(),
            att_out: core::ptr::null_mut(),
            hb1: core::ptr::null_mut(),
            hb3: core::ptr::null_mut(),
            h_glu: core::ptr::null_mut(),
            att_scores: core::ptr::null_mut(),
            key_cache: core::ptr::null_mut(),
            value_cache: core::ptr::null_mut(),
            logits: core::ptr::null_mut(),
            cur_layer: 0,
            cur_pos: 0,
            weights_ptr: core::ptr::null(),
            total_floats: 0,
            paddr: 0,
            page_count: 0,
        }
    }

    pub fn total_floats_for(cfg: &LlamaConfig) -> usize {
        let dim = cfg.dim;
        let hidden = cfg.hidden_dim;
        let layers = cfg.n_layers;
        let heads = cfg.n_heads;
        let seq = cfg.seq_len;
        let vocab = cfg.vocab_size;
        let kv_dim = if cfg.n_heads > 0 { (dim * cfg.n_kv_heads) / cfg.n_heads } else { dim };

        5 * dim + 2 * kv_dim        // x, xb, xb2, att_out (dim) + q (dim) + k (kv_dim) + v (kv_dim)
        + 3 * hidden                // hb1, hb3, h_glu
        + heads * seq               // att_scores
        + 2 * (layers * seq * kv_dim) // key_cache, value_cache
        + vocab                     // logits
    }

    pub unsafe fn init_from_buffer(&mut self, cfg: LlamaConfig, base_ptr: *mut f32) {
        self.config = cfg;
        let dim = cfg.dim;
        let hidden = cfg.hidden_dim;
        let layers = cfg.n_layers;
        let heads = cfg.n_heads;
        let seq = cfg.seq_len;
        let vocab = cfg.vocab_size;
        let kv_dim = if cfg.n_heads > 0 { (dim * cfg.n_kv_heads) / cfg.n_heads } else { dim };

        let mut p = base_ptr;
        self.x = p; p = p.add(dim);
        self.xb = p; p = p.add(dim);
        self.xb2 = p; p = p.add(dim);
        self.q = p; p = p.add(dim);
        self.k = p; p = p.add(kv_dim);
        self.v = p; p = p.add(kv_dim);
        self.att_out = p; p = p.add(dim);

        self.hb1 = p; p = p.add(hidden);
        self.hb3 = p; p = p.add(hidden);
        self.h_glu = p; p = p.add(hidden);

        self.att_scores = p; p = p.add(heads * seq);
        self.key_cache = p; p = p.add(layers * seq * kv_dim);
        self.value_cache = p; p = p.add(layers * seq * kv_dim);
        self.logits = p; p = p.add(vocab);

        self.total_floats = p.offset_from(base_ptr) as usize;
        core::ptr::write_bytes(base_ptr as *mut u8, 0, self.total_floats * 4);
    }

    pub unsafe fn logits_slice(&self) -> &[f32] {
        if self.logits.is_null() {
            &[]
        } else {
            core::slice::from_raw_parts(self.logits, self.config.vocab_size)
        }
    }
}

pub static mut SMP_LLAMA_CTX: SmpLlamaContext = SmpLlamaContext::empty();
pub static mut SMP_LLAMA_WEIGHTS: LlamaWeights = LlamaWeights::empty();

/// 4-core worker: Computes attention heads partitioned dynamically across cores (`cpu_id * heads_per_core`).
pub extern "C" fn smp_llama_attn_worker(cpu_id: u64, ctx_ptr: u64) {
    let ctx = unsafe { &mut *(ctx_ptr as *mut SmpLlamaContext) };
    let cfg = ctx.config;
    let l = ctx.cur_layer;
    let pos = ctx.cur_pos;
    let dim = cfg.dim;
    let n_heads = cfg.n_heads;
    let n_kv_heads = cfg.n_kv_heads;
    let seq_len = cfg.seq_len;
    if n_heads == 0 || dim == 0 || n_kv_heads == 0 {
        return;
    }
    let head_size = dim / n_heads;
    let kv_dim = (dim * n_kv_heads) / n_heads;
    let kv_mul = (n_heads / n_kv_heads).max(1);

    let heads_per_core = ((n_heads + 3) / 4).max(1);
    let h_start = ((cpu_id as usize) * heads_per_core).min(n_heads);
    let h_end = (h_start + heads_per_core).min(n_heads);
    let inv_sqrt_head = 1.0f32 / fast_sqrtf(head_size as f32);

    for h in h_start..h_end {
        let h_kv = h / kv_mul;
        let q_head = unsafe { core::slice::from_raw_parts(ctx.q.add(h * head_size), head_size) };
        let scores_base = h * seq_len;

        // 1. Calculate attention scores for t = 0..=pos
        let mut max_score = -1e9f32;
        for t in 0..=pos {
            let k_offset = l * (seq_len * kv_dim) + t * kv_dim + h_kv * head_size;
            let k_slice = unsafe { core::slice::from_raw_parts(ctx.key_cache.add(k_offset), head_size) };

            let mut dot = 0.0f32;
            for i in 0..head_size {
                dot += q_head[i] * k_slice[i];
            }
            let score = dot * inv_sqrt_head;
            unsafe { *ctx.att_scores.add(scores_base + t) = score; }
            if score > max_score {
                max_score = score;
            }
        }

        // 2. Softmax normalization
        let mut exp_sum = 0.0f32;
        for t in 0..=pos {
            let exp_val = fast_expf(unsafe { *ctx.att_scores.add(scores_base + t) } - max_score);
            unsafe { *ctx.att_scores.add(scores_base + t) = exp_val; }
            exp_sum += exp_val;
        }
        let inv_sum = 1.0f32 / (exp_sum.max(1e-5));
        for t in 0..=pos {
            unsafe { *ctx.att_scores.add(scores_base + t) *= inv_sum; }
        }

        // 3. Weighted sum of values into att_out slice
        let out_head = unsafe { core::slice::from_raw_parts_mut(ctx.att_out.add(h * head_size), head_size) };
        for i in 0..head_size {
            out_head[i] = 0.0;
        }

        for t in 0..=pos {
            let weight = unsafe { *ctx.att_scores.add(scores_base + t) };
            let v_offset = l * (seq_len * kv_dim) + t * kv_dim + h_kv * head_size;
            let v_slice = unsafe { core::slice::from_raw_parts(ctx.value_cache.add(v_offset), head_size) };
            for i in 0..head_size {
                out_head[i] += weight * v_slice[i];
            }
        }
    }
}

/// 4-core worker: Computes SwiGLU FFN units partitioned dynamically across cores (`cpu_id * slice_len`).
pub extern "C" fn smp_llama_ffn_worker(cpu_id: u64, ctx_ptr: u64) {
    let ctx = unsafe { &mut *(ctx_ptr as *mut SmpLlamaContext) };
    let weights = unsafe { &*ctx.weights_ptr };
    let cfg = ctx.config;
    let l = ctx.cur_layer;
    let dim = cfg.dim;
    let hidden = cfg.hidden_dim;
    if hidden == 0 || dim == 0 {
        return;
    }

    let slice_len = ((hidden + 3) / 4).max(1);
    let start = ((cpu_id as usize) * slice_len).min(hidden);
    let end = (start + slice_len).min(hidden);

    let w1_layer = unsafe { weights.w1.add(l * dim * hidden) };
    let w3_layer = unsafe { weights.w3.add(l * dim * hidden) };

    for j in start..end {
        let mut sum1 = 0.0f32;
        let mut sum3 = 0.0f32;
        for c in 0..dim {
            unsafe {
                sum1 += *ctx.xb.add(c) * *w1_layer.add(j * dim + c);
                sum3 += *ctx.xb.add(c) * *w3_layer.add(j * dim + c);
            }
        }
        let silu = sum1 / (1.0 + fast_expf(-sum1));
        unsafe {
            *ctx.h_glu.add(j) = silu * sum3;
        }
    }
}

/// Runs a single forward step of Llama-2 on token `token` at position `pos`.
pub fn run_llama_forward_step(
    weights: &LlamaWeights,
    ctx: &mut SmpLlamaContext,
    token: usize,
    pos: usize,
) {
    let cfg = ctx.config;
    let dim = cfg.dim;
    let hidden = cfg.hidden_dim;
    let layers = cfg.n_layers;
    let heads = cfg.n_heads;
    let n_kv_heads = cfg.n_kv_heads;
    let seq_len = cfg.seq_len;
    let head_size = dim / heads;
    let kv_dim = if heads > 0 { (dim * n_kv_heads) / heads } else { dim };
    let vocab = cfg.vocab_size;

    ctx.cur_pos = pos;
    ctx.weights_ptr = weights as *const LlamaWeights;

    // 1. Embedding lookup
    let emb_offset = (token % vocab) * dim;
    unsafe {
        core::ptr::copy_nonoverlapping(
            weights.token_embedding.add(emb_offset),
            ctx.x,
            dim,
        );
    }

    // 2. Transformer layers
    for l in 0..layers {
        ctx.cur_layer = l;

        // Attention RMSNorm
        let att_weight_offset = l * dim;
        unsafe {
            rmsnorm(
                core::slice::from_raw_parts_mut(ctx.xb, dim),
                core::slice::from_raw_parts(ctx.x, dim),
                core::slice::from_raw_parts(weights.rms_att.add(att_weight_offset), dim),
                1e-5,
            );
        }

        // Q, K, V projections
        let wq_offset = l * dim * dim;
        let wk_offset = l * dim * kv_dim;
        let wv_offset = l * dim * kv_dim;
        unsafe {
            let xb_slice = core::slice::from_raw_parts(ctx.xb, dim);
            matmul(core::slice::from_raw_parts_mut(ctx.q, dim), xb_slice, core::slice::from_raw_parts(weights.wq.add(wq_offset), dim * dim), dim, dim);
            matmul(core::slice::from_raw_parts_mut(ctx.k, kv_dim), xb_slice, core::slice::from_raw_parts(weights.wk.add(wk_offset), kv_dim * dim), kv_dim, dim);
            matmul(core::slice::from_raw_parts_mut(ctx.v, kv_dim), xb_slice, core::slice::from_raw_parts(weights.wv.add(wv_offset), kv_dim * dim), kv_dim, dim);
        }

        // RoPE rotations on Q and K
        unsafe {
            apply_rope(
                core::slice::from_raw_parts_mut(ctx.q, dim),
                core::slice::from_raw_parts_mut(ctx.k, kv_dim),
                pos,
                heads,
                n_kv_heads,
                head_size,
            );
        }

        // Save key and value to cache at position `pos`
        let kv_offset = l * (seq_len * kv_dim) + pos * kv_dim;
        unsafe {
            core::ptr::copy_nonoverlapping(ctx.k, ctx.key_cache.add(kv_offset), kv_dim);
            core::ptr::copy_nonoverlapping(ctx.v, ctx.value_cache.add(kv_offset), kv_dim);
        }

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
        let wo_offset = l * dim * dim;
        unsafe {
            matmul(
                core::slice::from_raw_parts_mut(ctx.xb2, dim),
                core::slice::from_raw_parts(ctx.att_out, dim),
                core::slice::from_raw_parts(weights.wo.add(wo_offset), dim * dim),
                dim,
                dim,
            );
        }

        // Residual connection: x = x + xb2
        unsafe {
            for i in 0..dim {
                *ctx.x.add(i) += *ctx.xb2.add(i);
            }
        }

        // FFN RMSNorm
        unsafe {
            rmsnorm(
                core::slice::from_raw_parts_mut(ctx.xb, dim),
                core::slice::from_raw_parts(ctx.x, dim),
                core::slice::from_raw_parts(weights.rms_ffn.add(att_weight_offset), dim),
                1e-5,
            );
        }

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
        let w2_offset = l * hidden * dim;
        unsafe {
            matmul(
                core::slice::from_raw_parts_mut(ctx.xb2, dim),
                core::slice::from_raw_parts(ctx.h_glu, hidden),
                core::slice::from_raw_parts(weights.w2.add(w2_offset), dim * hidden),
                dim,
                hidden,
            );
        }

        // Residual connection: x = x + xb2
        unsafe {
            for i in 0..dim {
                *ctx.x.add(i) += *ctx.xb2.add(i);
            }
        }
    }

    // 3. Final RMSNorm
    unsafe {
        rmsnorm(
            core::slice::from_raw_parts_mut(ctx.xb, dim),
            core::slice::from_raw_parts(ctx.x, dim),
            core::slice::from_raw_parts(weights.rms_final, dim),
            1e-5,
        );
    }

    // 4. Classifier / LM Head projection onto vocabulary
    unsafe {
        for v in 0..vocab {
            let emb_offset = v * dim;
            let mut logit = 0.0f32;
            for i in 0..dim {
                logit += *ctx.xb.add(i) * *weights.wcls.add(emb_offset + i);
            }
            *ctx.logits.add(v) = logit;
        }
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

fn xorshift64(state: &mut u64) -> u64 {
    let mut x = *state;
    if x == 0 {
        x = 0x853c_49e6_748f_ea9b;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

fn rand_uniform(state: &mut u64) -> f32 {
    let u = (xorshift64(state) >> 40) as u32;
    (u as f32) / 16777216.0f32
}

/// Temperature and top-p (nucleus) sampler.
/// When `temperature <= 0.0`, falls back to greedy argmax.
pub fn sample(
    logits: &mut [f32],
    temperature: f32,
    top_p: f32,
    rng_state: &mut u64,
) -> usize {
    if temperature <= 0.0 || logits.is_empty() {
        return sample_greedy(logits);
    }

    let inv_temp = 1.0f32 / temperature;
    for val in logits.iter_mut() {
        *val *= inv_temp;
    }

    // Softmax
    let mut max_val = -1e9f32;
    for &val in logits.iter() {
        if val > max_val {
            max_val = val;
        }
    }
    let mut sum = 0.0f32;
    for val in logits.iter_mut() {
        *val = fast_expf(*val - max_val);
        sum += *val;
    }
    let inv_sum = 1.0f32 / sum.max(1e-5);
    for val in logits.iter_mut() {
        *val *= inv_sum;
    }

    let coin = rand_uniform(rng_state);

    if top_p > 0.0 && top_p < 1.0 {
        let mut sorted_indices = [0usize; 128];
        let n = logits.len().min(128);
        for i in 0..n {
            sorted_indices[i] = i;
        }
        for i in 1..n {
            let key = sorted_indices[i];
            let key_p = logits[key];
            let mut j = i;
            while j > 0 && logits[sorted_indices[j - 1]] < key_p {
                sorted_indices[j] = sorted_indices[j - 1];
                j -= 1;
            }
            sorted_indices[j] = key;
        }

        let mut cumulative_p = 0.0f32;
        let mut cutoff_idx = n;
        for i in 0..n {
            cumulative_p += logits[sorted_indices[i]];
            if cumulative_p >= top_p {
                cutoff_idx = i + 1;
                break;
            }
        }

        let top_sum = cumulative_p.max(1e-5);
        let target_coin = coin * top_sum;
        let mut acc = 0.0f32;
        for i in 0..cutoff_idx {
            let idx = sorted_indices[i];
            acc += logits[idx];
            if acc >= target_coin {
                return idx;
            }
        }
        sorted_indices[cutoff_idx.saturating_sub(1)]
    } else {
        let mut cdf = 0.0f32;
        for (idx, &p) in logits.iter().enumerate() {
            cdf += p;
            if cdf >= coin {
                return idx;
            }
        }
        logits.len() - 1
    }
}

/// Parses a `llama2.c` binary format header.
pub fn parse_llama_header(data: &[u8]) -> Result<LlamaConfig, &'static str> {
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
    let raw_vocab = read_i32(20);
    let shared_weights = raw_vocab > 0;
    let vocab_size = raw_vocab.unsigned_abs() as usize;
    let seq_len = read_i32(24) as usize;

    if dim == 0 || n_heads == 0 || n_kv_heads == 0 || dim % n_heads != 0 || n_heads % n_kv_heads != 0 {
        return Err("Invalid Llama dimensions: dim must be divisible by n_heads, and n_heads divisible by n_kv_heads");
    }

    Ok(LlamaConfig {
        dim,
        hidden_dim,
        n_layers,
        n_heads,
        n_kv_heads,
        vocab_size,
        seq_len,
        shared_weights,
    })
}

/// Copies model weights from `data` into an already initialized `LlamaWeights` buffer.
pub fn load_llama_weights(data: &[u8], weights: &mut LlamaWeights) -> Result<(), &'static str> {
    let cfg = parse_llama_header(data)?;
    let required_floats = LlamaWeights::total_floats_for(&cfg);
    if weights.total_floats < required_floats || weights.token_embedding.is_null() {
        return Err("Weights buffer not initialized or too small for model");
    }

    let byte_len = required_floats * 4;
    if data.len() < 28 + byte_len {
        return Err("Unexpected EOF reading model weights");
    }

    unsafe {
        core::ptr::copy_nonoverlapping(
            data.as_ptr().add(28),
            weights.token_embedding as *mut u8,
            byte_len,
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use alloc::vec;
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
        assert_eq!(out[2], 0.0);
        assert!(out[0] > 0.0);
    }

    #[test]
    fn dynamic_llama_forward_step_test() {
        let cfg = LlamaConfig {
            dim: 64,
            hidden_dim: 128,
            n_layers: 2,
            n_heads: 4,
            n_kv_heads: 4,
            vocab_size: 128,
            seq_len: 32,
            shared_weights: true,
        };

        let weight_floats = LlamaWeights::total_floats_for(&cfg);
        let mut weight_buf = vec![0.01f32; weight_floats];
        let mut weights = LlamaWeights::empty();
        unsafe {
            weights.init_from_buffer(cfg, weight_buf.as_mut_ptr());
        }

        let ctx_floats = SmpLlamaContext::total_floats_for(&cfg);
        let mut ctx_buf = vec![0.0f32; ctx_floats];
        let mut ctx = SmpLlamaContext::empty();
        unsafe {
            ctx.init_from_buffer(cfg, ctx_buf.as_mut_ptr());
        }

        run_llama_forward_step(&weights, &mut ctx, 65, 0);
        let tok = sample_greedy(unsafe { ctx.logits_slice() });
        assert!(tok < cfg.vocab_size);
    }

    #[test]
    fn dynamic_llama_larger_dim_and_heads() {
        let cfg = LlamaConfig {
            dim: 128,
            hidden_dim: 256,
            n_layers: 1,
            n_heads: 8,
            n_kv_heads: 8,
            vocab_size: 64,
            seq_len: 16,
            shared_weights: true,
        };

        let weight_floats = LlamaWeights::total_floats_for(&cfg);
        let mut weight_buf = vec![0.02f32; weight_floats];
        let mut weights = LlamaWeights::empty();
        unsafe {
            weights.init_from_buffer(cfg, weight_buf.as_mut_ptr());
        }

        let ctx_floats = SmpLlamaContext::total_floats_for(&cfg);
        let mut ctx_buf = vec![0.0f32; ctx_floats];
        let mut ctx = SmpLlamaContext::empty();
        unsafe {
            ctx.init_from_buffer(cfg, ctx_buf.as_mut_ptr());
        }

        run_llama_forward_step(&weights, &mut ctx, 10, 0);
        let tok = sample_greedy(unsafe { ctx.logits_slice() });
        assert!(tok < cfg.vocab_size);
    }

    #[test]
    fn llama_gqa_and_unshared_weights_test() {
        // GQA: 8 query heads, 2 KV heads (kv_mul = 4)
        // unshared weights: shared_weights = false
        let cfg = LlamaConfig {
            dim: 64,
            hidden_dim: 128,
            n_layers: 2,
            n_heads: 8,
            n_kv_heads: 2,
            vocab_size: 64,
            seq_len: 16,
            shared_weights: false,
        };

        let weight_floats = LlamaWeights::total_floats_for(&cfg);
        let mut weight_buf = vec![0.015f32; weight_floats];
        let mut weights = LlamaWeights::empty();
        unsafe {
            weights.init_from_buffer(cfg, weight_buf.as_mut_ptr());
        }

        let ctx_floats = SmpLlamaContext::total_floats_for(&cfg);
        let mut ctx_buf = vec![0.0f32; ctx_floats];
        let mut ctx = SmpLlamaContext::empty();
        unsafe {
            ctx.init_from_buffer(cfg, ctx_buf.as_mut_ptr());
        }

        run_llama_forward_step(&weights, &mut ctx, 12, 0);
        let tok = sample_greedy(unsafe { ctx.logits_slice() });
        assert!(tok < cfg.vocab_size);
    }

    #[test]
    fn llama_ceiling_division_non_multiple_of_4_heads() {
        // 6 query heads, 2 KV heads (kv_mul = 3)
        let cfg = LlamaConfig {
            dim: 48,
            hidden_dim: 96,
            n_layers: 1,
            n_heads: 6,
            n_kv_heads: 2,
            vocab_size: 32,
            seq_len: 8,
            shared_weights: true,
        };

        let weight_floats = LlamaWeights::total_floats_for(&cfg);
        let mut weight_buf = vec![0.01f32; weight_floats];
        let mut weights = LlamaWeights::empty();
        unsafe {
            weights.init_from_buffer(cfg, weight_buf.as_mut_ptr());
        }

        let ctx_floats = SmpLlamaContext::total_floats_for(&cfg);
        let mut ctx_buf = vec![0.0f32; ctx_floats];
        let mut ctx = SmpLlamaContext::empty();
        unsafe {
            ctx.init_from_buffer(cfg, ctx_buf.as_mut_ptr());
        }

        run_llama_forward_step(&weights, &mut ctx, 5, 0);
        let tok = sample_greedy(unsafe { ctx.logits_slice() });
        assert!(tok < cfg.vocab_size);
    }

    #[test]
    fn llama_temperature_and_topp_sampling_test() {
        let logits = [1.0f32, 5.0, 2.0, 0.5];
        let mut rng = 123456789u64;

        // Greedy test
        let tok_greedy = sample_greedy(&logits);
        assert_eq!(tok_greedy, 1);

        // Temperature 0.0 falls back to greedy
        let tok_zero_temp = sample(&mut logits.clone(), 0.0, 0.9, &mut rng);
        assert_eq!(tok_zero_temp, 1);

        // Temperature > 0 sampling produces a valid token
        let tok_sampled = sample(&mut logits.clone(), 0.8, 0.9, &mut rng);
        assert!(tok_sampled < logits.len());
    }
}
