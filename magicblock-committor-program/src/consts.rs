/// Max bytes that can be allocated as part of the one instruction.
/// For buffers that are larger than that ReallocBuffer needs to be
/// invoked 1 or more times after Init completed.
pub const MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE: u16 = 10_240;

/// The maximum number of instructions that can be added to a single transaction.
/// See: https://github.com/solana-labs/solana/issues/33863
pub const MAX_INSTRUCTION_TRACE_LENGTH: u8 = 64;

/// We ran into max transaction size exceeded if we included more than
/// the below amount of instructions in a single transaction.
/// (VersionedTransaction too large: xxxx bytes (max: encoded/raw 1644/1232))
/// Thus the [MAX_INSTRUCTION_TRACE_LENGTH] is not the upper limit, but we're
/// capped by the size of each instruction. (see [crate::instruction_chunks::chunk_realloc_ixs])
pub const MAX_INSTRUCTION_LENGTH: u8 = 11;

/// This size is based on exploration of the Write instruction of the BPFUpgradableLoader program
///
/// It includes the following accounts:
///
/// - account
/// - authority
///
/// The write instruction:
///
/// ```rust
/// pub enum UpgradeableLoaderInstruction {
///     Write {
///         /// Offset at which to write the given bytes.
///         offset: u32,
///         /// Serialized program data
///         bytes: Vec<u8>,
///     }
/// }
/// ```
///
/// The instruction data size total I measured was 1028 bytes
/// The bytes hold 1012 bytes(see tools/sh/deploy-ix-bytesize)
/// which leaves 16 bytes for:
/// - offset:                                                   4 bytes
/// - instruction discriminator: 1 byte aligned to              4 bytes
/// - both accounts repeated in instruction: 2x 4 bytes         8 bytes
pub const MAX_INSTRUCTION_DATA_SIZE: u16 = 1028;
