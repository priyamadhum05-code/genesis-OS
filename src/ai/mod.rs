//! Project Genesis — AI & Vectorized Compute Subsystem.

pub mod matmul;

pub use matmul::{
    neon_matmul_slice, scalar_matmul, smp_matmul_worker, verify_matrices, MATMUL_N,
};
