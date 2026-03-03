use solana_program_runtime::{
    invoke_context::InvokeContext,
    solana_sbpf::{
        declare_builtin_function,
        memory_region::{AccessType, MemoryMapping},
    },
};

const CU_BASE: u64 = 100;
const CU_PER_MAC: u64 = 1;

fn map_mem(
    mm: &MemoryMapping,
    access: AccessType,
    addr: u64,
    len: u64,
) -> Result<u64, Box<dyn std::error::Error>> {
    Result::from(mm.map(access, addr, len)).map_err(|e| format!("{e:?}").into())
}

/// INT8 matrix-vector multiply: y[i] = sum(W[i][j] * x[j]) for j in 0..cols.
fn matmul_i8(
    weights: &[i8],
    input: &[i8],
    output: &mut [i32],
    rows: usize,
    cols: usize,
) {
    assert!(weights.len() >= rows * cols);
    assert!(input.len() >= cols);
    assert!(output.len() >= rows);

    for (i, out) in output.iter_mut().take(rows).enumerate() {
        let mut acc: i32 = 0;
        let row_start = i * cols;
        for j in 0..cols {
            acc += weights[row_start + j] as i32 * input[j] as i32;
        }
        *out = acc;
    }
}

declare_builtin_function!(
    /// Native INT8 matrix-vector multiply syscall.
    ///
    /// Register mapping:
    /// r1: VM pointer to row-major i8 weights [rows * cols]
    /// r2: VM pointer to i8 input vector [cols]
    /// r3: VM pointer to caller-allocated i32 output [rows]
    /// r4: rows
    /// r5: cols
    SyscallMatmulI8,
    fn rust(
        invoke_context: &mut InvokeContext,
        weights_addr: u64,
        input_addr: u64,
        output_addr: u64,
        rows: u64,
        cols: u64,
        memory_mapping: &mut MemoryMapping,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        let rows_usize = usize::try_from(rows).map_err(|_| "rows out of range")?;
        let cols_usize = usize::try_from(cols).map_err(|_| "cols out of range")?;

        let macs = rows
            .checked_mul(cols)
            .ok_or("matmul dimensions overflow")?;
        let cu_cost = CU_BASE.saturating_add(macs.saturating_mul(CU_PER_MAC));
        invoke_context.consume_checked(cu_cost)?;

        if rows_usize == 0 {
            return Ok(0);
        }

        let output_len = rows.checked_mul(4).ok_or("output length overflow")?;
        let output_host =
            map_mem(memory_mapping, AccessType::Store, output_addr, output_len)?;

        // SAFETY: memory_mapping.map() validates pointer bounds and access.
        let output =
            unsafe { std::slice::from_raw_parts_mut(output_host as *mut i32, rows_usize) };
        output.fill(0);

        if cols_usize == 0 {
            return Ok(0);
        }

        let weights_count = rows_usize
            .checked_mul(cols_usize)
            .ok_or("weights length overflow")?;
        let weights_len =
            u64::try_from(weights_count).map_err(|_| "weights length overflow")?;

        let weights_host =
            map_mem(memory_mapping, AccessType::Load, weights_addr, weights_len)?;
        let input_host = map_mem(memory_mapping, AccessType::Load, input_addr, cols)?;

        // SAFETY: memory_mapping.map() validates pointer bounds and access.
        let weights =
            unsafe { std::slice::from_raw_parts(weights_host as *const i8, weights_count) };
        let input = unsafe { std::slice::from_raw_parts(input_host as *const i8, cols_usize) };

        matmul_i8(weights, input, output, rows_usize, cols_usize);

        Ok(0)
    }
);

#[cfg(test)]
mod tests {
    use solana_program_runtime::{
        invoke_context::InvokeContext, solana_sbpf::program::BuiltinProgram,
    };

    use super::{matmul_i8, SyscallMatmulI8};

    #[test]
    fn matmul_2x2_known_values() {
        // [[1,2],[3,4]] x [5,6] = [17, 39]
        let rows = 2;
        let cols = 2;
        let weights = [1, 2, 3, 4];
        let input = [5, 6];
        let mut output = [0i32; 2];

        matmul_i8(&weights, &input, &mut output, rows, cols);

        assert_eq!(output, [17, 39]);
    }

    #[test]
    fn matmul_negative_values() {
        // [[-1,2],[3,-4]] x [-5,6] = [17, -39]
        let rows = 2;
        let cols = 2;
        let weights = [-1, 2, 3, -4];
        let input = [-5, 6];
        let mut output = [0i32; 2];

        matmul_i8(&weights, &input, &mut output, rows, cols);

        assert_eq!(output[0], 17);
        assert_eq!(output[1], -39);
    }

    #[test]
    fn matmul_larger_matrix() {
        let rows = 4usize;
        let cols = 8usize;
        let weights: Vec<i8> = (0..rows * cols)
            .map(|i| ((i * 3 + 7) % 256) as i8)
            .collect();
        let input: Vec<i8> =
            (0..cols).map(|i| ((i * 5 + 1) % 256) as i8).collect();
        let mut output = vec![0i32; rows];

        matmul_i8(&weights, &input, &mut output, rows, cols);

        for i in 0..rows {
            let expected: i32 = (0..cols)
                .map(|j| weights[i * cols + j] as i32 * input[j] as i32)
                .sum();
            assert_eq!(output[i], expected, "row {i} mismatch");
        }
    }

    #[test]
    fn registers_sol_matmul_i8_syscall() {
        let mut runtime: BuiltinProgram<InvokeContext<'_>> =
            BuiltinProgram::new_loader(Default::default());
        runtime
            .register_function("sol_matmul_i8", SyscallMatmulI8::vm)
            .expect("failed to register sol_matmul_i8");

        let found = runtime
            .get_function_registry()
            .iter()
            .any(|(_, (name, _))| name == b"sol_matmul_i8");

        assert!(found, "sol_matmul_i8 missing from function registry");
    }
}
