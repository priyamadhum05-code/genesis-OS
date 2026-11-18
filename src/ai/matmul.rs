//! Project Genesis — NEON-Accelerated Matrix Multiplication Kernel.
//!
//! Provides vectorized f32 matrix multiply using AArch64 NEON SIMD intrinsics,
//! partitioned across multi-core SMP workers.

pub const MATMUL_N: usize = 64; // 64x64 matrix

/// Computes a slice of rows for C = A * B using AArch64 NEON SIMD.
/// `start_row..end_row` specifies the partition for the executing core.
pub fn neon_matmul_slice(
    a: &[f32],
    b: &[f32],
    c: &mut [f32],
    n: usize,
    start_row: usize,
    end_row: usize,
) {
    for i in start_row..end_row {
        let a_row = &a[i * n..(i + 1) * n];
        let c_row = &mut c[i * n..(i + 1) * n];

        // Zero output row
        for val in c_row.iter_mut() {
            *val = 0.0;
        }

        for k in 0..n {
            let a_val = a_row[k];
            let b_row = &b[k * n..(k + 1) * n];

            #[cfg(target_arch = "aarch64")]
            unsafe {
                use core::arch::aarch64::*;
                let a_vec = vdupq_n_f32(a_val);
                for j in (0..n).step_by(4) {
                    let b_ptr = b_row.as_ptr().add(j);
                    let c_ptr = c_row.as_mut_ptr().add(j);

                    let b_vec = vld1q_f32(b_ptr);
                    let c_vec = vld1q_f32(c_ptr);
                    let res = vfmaq_f32(c_vec, b_vec, a_vec);
                    vst1q_f32(c_ptr, res);
                }
            }

            #[cfg(not(target_arch = "aarch64"))]
            {
                for j in 0..n {
                    c_row[j] += a_val * b_row[j];
                }
            }
        }
    }
}

/// Scalar reference matrix multiply for numerical verification.
pub fn scalar_matmul(a: &[f32], b: &[f32], c: &mut [f32], n: usize) {
    for i in 0..n {
        for j in 0..n {
            let mut sum = 0.0f32;
            for k in 0..n {
                sum += a[i * n + k] * b[k * n + j];
            }
            c[i * n + j] = sum;
        }
    }
}

/// Compares two matrices within epsilon.
pub fn verify_matrices(c_neon: &[f32], c_ref: &[f32], n: usize) -> bool {
    for i in 0..n * n {
        let diff = (c_neon[i] - c_ref[i]).abs();
        if diff > 1e-3 {
            return false;
        }
    }
    true
}

/// SMP worker entry point for parallel matmul.
/// `cpu_id`: 0..=3
/// `hugepage_va`: Base VA of the 2 MiB hugepage where matrices are mapped:
/// - A at offset 0x0000 (16 KiB = 4096 f32)
/// - B at offset 0x4000 (16 KiB = 4096 f32)
/// - C at offset 0x8000 (16 KiB = 4096 f32)
#[no_mangle]
pub extern "C" fn smp_matmul_worker(cpu_id: u64, hugepage_va: u64) {
    let n = MATMUL_N;
    let rows_per_core = n / 4;
    let start_row = (cpu_id as usize) * rows_per_core;
    let end_row = start_row + rows_per_core;

    let a_ptr = hugepage_va as *const f32;
    let b_ptr = (hugepage_va + 0x4000) as *const f32;
    let c_ptr = (hugepage_va + 0x8000) as *mut f32;

    let a = unsafe { core::slice::from_raw_parts(a_ptr, n * n) };
    let b = unsafe { core::slice::from_raw_parts(b_ptr, n * n) };
    let c = unsafe { core::slice::from_raw_parts_mut(c_ptr, n * n) };

    neon_matmul_slice(a, b, c, n, start_row, end_row);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neon_and_scalar_matmul_equivalence() {
        let n = 16;
        let mut a = [0.0f32; 256];
        let mut b = [0.0f32; 256];
        let mut c_neon = [0.0f32; 256];
        let mut c_ref = [0.0f32; 256];

        for i in 0..256 {
            a[i] = (i as f32) * 0.1;
            b[i] = ((255 - i) as f32) * 0.05;
        }

        // Test partitioned neon slices
        let rows_per_core = n / 4;
        for core in 0..4 {
            let start = core * rows_per_core;
            let end = start + rows_per_core;
            neon_matmul_slice(&a, &b, &mut c_neon, n, start, end);
        }

        scalar_matmul(&a, &b, &mut c_ref, n);
        assert!(verify_matrices(&c_neon, &c_ref, n));
    }
}
