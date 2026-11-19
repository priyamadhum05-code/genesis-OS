//! Project Genesis — AI & Vectorized Compute Subsystem.

pub mod matmul;
pub mod nn;
pub mod transformer;
pub mod llm;
pub mod llama;

pub use llama::{
    fast_expf, fast_sin_cos, fast_sqrtf, load_llama_weights, matmul as llama_matmul, rmsnorm,
    run_llama_forward_step, sample_greedy, smp_llama_attn_worker, smp_llama_ffn_worker,
    LlamaConfig, LlamaWeights, SmpLlamaContext, LLAMA_DIM, LLAMA_HEADS, LLAMA_HEAD_SIZE,
    LLAMA_HIDDEN_DIM, LLAMA_KV_HEADS, LLAMA_LAYERS, LLAMA_SEQ_LEN, LLAMA_VOCAB,
    SMP_LLAMA_CTX, SMP_LLAMA_WEIGHTS,
};

pub use matmul::{
    neon_matmul_slice, scalar_matmul, smp_matmul_worker, verify_matrices, MATMUL_N,
};
pub use nn::{
    load_model_from_bytes, neon_dot_product_64, run_parallel_nn_inference, scalar_dot_product_64,
    scalar_infer, smp_nn_layer1_worker, verify_inference, SmpNnContext, NN_HIDDEN_SIZE,
    NN_INPUT_SIZE, NN_MODEL_MAGIC, NN_OUTPUT_SIZE, SMP_NN_CTX,
};
pub use transformer::{
    load_transformer_from_bytes, run_parallel_transformer, scalar_transformer_block,
    smp_transformer_attn_ffn_worker, smp_transformer_qkv_worker, verify_transformer,
    SmpTransformerContext, SMP_TRANSFORMER_CTX, TRANSFORMER_D_FF, TRANSFORMER_D_MODEL,
    TRANSFORMER_MAGIC, TRANSFORMER_SEQ_LEN,
};
pub use llm::{
    load_micro_llm_from_bytes, run_parallel_micro_llm_step, scalar_micro_llm_step,
    verify_llm_step, MicroLlmWeights, TransformerBlockWeights, MICRO_LLM_D_FF,
    MICRO_LLM_D_MODEL, MICRO_LLM_LAYERS, MICRO_LLM_MAGIC, MICRO_LLM_SEQ_LEN,
    MICRO_LLM_VOCAB, SMP_MICRO_LLM_WEIGHTS,
};
