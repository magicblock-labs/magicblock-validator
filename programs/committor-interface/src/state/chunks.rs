use std::{collections::HashSet, fmt};

use borsh::{BorshDeserialize, BorshSerialize};

use crate::consts;

const BITS_PER_BYTE: usize = 8;

/// A bitfield based implementation to keep track of which chunks have been delivered.
/// This is much more memory efficient than a Vec<bool> which uses 1 byte per value.
/// [https://doc.rust-lang.org/reference/type-layout.html#r-layout.primitive.size]
#[derive(Debug, Clone, BorshSerialize, BorshDeserialize)]
pub struct Chunks {
    /// The bitfields tracking chunk state.
    bits: Vec<u8>,
    /// The tracking capacity which is
    /// ```rust
    /// # const BIT_FIELD_SIZE: usize = 8;
    /// # let bits = vec![0u8; 1];
    /// let capacity = bits.len() * BIT_FIELD_SIZE;
    /// ```
    /// The amount of tracked chunks could be a bit smaller as it might only use
    /// part of the last bit in [Chunks::bits].
    /// This count gives that smaller amount.
    count: usize,
    /// The size of chunks that we are tracking.
    chunk_size: u16,
}

impl Chunks {
    pub fn new(chunk_count: usize, chunk_size: u16) -> Self {
        // SAFETY: this is a bug and we need to crash and burn
        assert!(
            Self::struct_size(chunk_count)
                < consts::MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE as usize,
            "Size ({}) needed to track {} chunks is too large track and would require to realloc. Max allowed is {} bytes",
            Self::struct_size(chunk_count),
            chunk_count,
            consts::MAX_ACCOUNT_ALLOC_PER_INSTRUCTION_SIZE
        );
        Self {
            bits: vec![0; Self::count_to_bitfield_bytes(chunk_count)],
            count: chunk_count,
            chunk_size,
        }
    }

    pub fn from_data_length(data_len: usize, chunk_size: u16) -> Self {
        // A chunk size of 0 means "do not chunk this account", so there is
        // nothing to track. Guard it here as well, since `div_ceil` would
        // otherwise divide by zero.
        let chunk_count = match chunk_size {
            0 => 0,
            chunk_size => data_len.div_ceil(chunk_size as usize),
        };
        Self::new(chunk_count, chunk_size)
    }

    /// Calculates the minimum number of bytes needed to store `count` boolean values in a bitfield.
    ///
    /// Each boolean is stored as a single bit, packing 8 booleans per byte.
    /// Returns the number of bytes needed to store all flags, rounding up if necessary.
    fn count_to_bitfield_bytes(count: usize) -> usize {
        count.div_ceil(BITS_PER_BYTE)
    }

    /// Returns the Borsh size of [`Chunks`] for a given chunk count.
    pub fn struct_size(count: usize) -> usize {
        // Vector length prefix, bits, usize encoded as u64, and chunk size.
        4 + Self::count_to_bitfield_bytes(count) + size_of::<u64>() + size_of::<u16>()
    }

    /// Borsh length, including the vector's four-byte length prefix.
    pub fn serialized_size(&self) -> usize {
        Self::struct_size(self.count)
    }

    /// Returns `true` if the chunk at index has been delivered
    pub fn is_chunk_delivered(&self, idx: usize) -> Option<bool> {
        if idx >= self.count {
            None
        } else {
            let vec_idx = idx / BITS_PER_BYTE;
            let bit_idx = idx % BITS_PER_BYTE;
            Some((self.bits[vec_idx] & (1 << bit_idx)) != 0)
        }
    }

    /// Sets the chunk at index to `true` denoting that it has been delivered
    pub(super) fn set_chunk_delivered(&mut self, idx: usize) -> Result<(), ChunksError> {
        if idx < self.count {
            let vec_idx = idx / BITS_PER_BYTE;
            let bit_idx = idx % BITS_PER_BYTE;
            self.bits[vec_idx] |= 1 << bit_idx;
            Ok(())
        } else {
            Err(ChunksError::OutOfBoundsError)
        }
    }

    /// Marks that chunk at offset was written to
    pub fn set_offset_delivered(&mut self, offset: usize) -> Result<(), ChunksError> {
        let chunk_size = self.checked_chunk_size()?;
        if offset % chunk_size != 0 {
            Err(ChunksError::InvalidOffsetError(offset, self.chunk_size))
        } else {
            let idx = offset / chunk_size;
            self.set_chunk_delivered(idx)?;
            Ok(())
        }
    }

    /// Return [`true`] if offset delivered
    /// Returns error if offset isn't multuple of chunk
    pub fn is_offset_delivered(&self, offset: usize) -> Result<bool, ChunksError> {
        let chunk_size = self.checked_chunk_size()?;
        if offset % chunk_size != 0 {
            return Err(ChunksError::InvalidOffsetError(offset, self.chunk_size));
        }
        let idx = offset / chunk_size;
        self.is_chunk_delivered(idx).ok_or(ChunksError::OutOfBoundsError)
    }

    /// Returns the chunk size as a `usize`, rejecting the `0` sentinel that
    /// means "not chunked". Without this guard the offset helpers below would
    /// divide by zero and abort the program.
    fn checked_chunk_size(&self) -> Result<usize, ChunksError> {
        if self.chunk_size == 0 {
            Err(ChunksError::InvalidChunkSize)
        } else {
            Ok(self.chunk_size as usize)
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn chunk_size(&self) -> u16 {
        self.chunk_size
    }

    // Every index below count is addressable in the bitfield created by new.
    #[allow(clippy::expect_used)]
    pub fn get_missing_chunks(&self) -> HashSet<usize> {
        (0..self.count)
            .filter(|&i| !self.is_chunk_delivered(i).expect("invariant"))
            .collect()
    }

    pub fn is_complete(&self) -> bool {
        self.get_missing_chunks().is_empty()
    }
}

impl From<(Vec<bool>, u16)> for Chunks {
    // The constructor creates exactly one bit for every input element.
    #[allow(clippy::expect_used)]
    fn from((vec, chunk_size): (Vec<bool>, u16)) -> Self {
        let mut this = Chunks::new(vec.len(), chunk_size);
        vec.into_iter().enumerate().for_each(|(i, d)| {
            if d {
                this.set_chunk_delivered(i).expect("invariant");
            }
        });

        this
    }
}

impl fmt::Display for Chunks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (idx, bit) in self.bits.iter().enumerate() {
            if idx % 8 == 0 {
                write!(f, "\n{:05}: ", idx * BITS_PER_BYTE)?;
            }
            let bit = format!("{:08b}", bit);
            let bit = bit.chars().rev().collect::<String>();
            // add space after 4 bits
            let (bit1, bit2) = bit.split_at(4);
            write!(f, "{} {} ", bit1, bit2)?;
        }
        Ok(())
    }
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum ChunksError {
    #[error("Out of bounds access")]
    OutOfBoundsError,
    #[error("Offset ({0}) must be multiple of chunk size ({1})")]
    InvalidOffsetError(usize, u16),
    #[error("Chunk size must be non-zero to address chunks by offset")]
    InvalidChunkSize,
}

#[cfg(test)]
mod test {
    use super::*;

    impl Chunks {
        pub(super) fn iter(&self) -> ChunksIter<'_> {
            ChunksIter { chunks: self, idx: 0 }
        }
    }

    pub(super) struct ChunksIter<'a> {
        chunks: &'a Chunks,
        idx: usize,
    }

    impl Iterator for ChunksIter<'_> {
        type Item = bool;
        fn next(&mut self) -> Option<Self::Item> {
            if self.idx < self.chunks.count {
                let idx = self.idx;
                self.idx += 1;
                Some(self.chunks.is_chunk_delivered(idx).expect("invariant"))
            } else {
                None
            }
        }
    }

    const CHUNK_SIZE: u16 = 128;

    #[test]
    fn test_chunks_iter() {
        let chunks = vec![true, false, false, false];
        let chunks = Chunks::from((chunks, CHUNK_SIZE));
        let vec = chunks.iter().collect::<Vec<_>>();
        assert_eq!(vec, vec![true, false, false, false]);
    }

    #[test]
    fn test_chunks_set_get_idx() {
        let chunks = vec![false; 12];
        let mut chunks = Chunks::from((chunks, CHUNK_SIZE));
        assert!(chunks.set_chunk_delivered(0).is_ok());
        assert!(chunks.set_chunk_delivered(10).is_ok());

        assert!(chunks.is_chunk_delivered(0).unwrap());
        assert!(!chunks.is_chunk_delivered(1).unwrap());
        assert!(chunks.is_chunk_delivered(10).unwrap());

        let vec = chunks.iter().collect::<Vec<_>>();
        #[rustfmt::skip]
        assert_eq!(
            vec,
            vec![
                true, false, false, false, false, false, false, false,
                false, false, true, false
            ]
        );
    }

    #[test]
    fn test_chunks_zero_chunk_size_is_rejected_not_divided_by() {
        let chunks = Chunks::from_data_length(547, 0);

        assert_eq!(chunks.count(), 0);
        assert!(chunks.get_missing_chunks().is_empty());
        assert_eq!(chunks.set_offset_delivered(0), Err(ChunksError::InvalidChunkSize));
        assert_eq!(
            chunks.is_offset_delivered(0),
            Err(ChunksError::InvalidChunkSize)
        );
    }

    #[test]
    fn test_chunks_set_get_idx_large() {
        let chunks = vec![false; 2048];
        let mut chunks = Chunks::from((chunks, CHUNK_SIZE));
        assert!(chunks.set_chunk_delivered(99).is_ok());
        assert!(chunks.set_chunk_delivered(1043).is_ok());

        assert!(!chunks.is_chunk_delivered(0).unwrap());
        assert!(!chunks.is_chunk_delivered(1).unwrap());
        assert!(chunks.is_chunk_delivered(99).unwrap());
        assert!(!chunks.is_chunk_delivered(1042).unwrap());
        assert!(chunks.is_chunk_delivered(1043).unwrap());
        assert!(!chunks.is_chunk_delivered(1044).unwrap());

        // Out of bound request
        assert!(chunks.is_chunk_delivered(2048).is_none());
        assert!(chunks.is_chunk_delivered(2049).is_none());

        assert_eq!(chunks.iter().count(), 2048);
    }
}
