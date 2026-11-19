//! Project Genesis — AI & Vectorized Compute Subsystem.

pub mod matmul;
pub mod nn;
pub mod transformer;

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
