use std::{collections::HashSet, fmt};

use borsh::{BorshDeserialize, BorshSerialize};

use crate::{
    consts,
    error::{CommittorError, CommittorResult},
};

const BIT_FIELD_SIZE: usize = 8;

/// A bitfield based implementation to keep track of which chunks have been delivered.
/// This is much more memory efficient than a Vec<bool> which uses 1 byte per value.
/// [https://doc.rust-lang.org/reference/type-layout.html#r-layout.primitive.size]
#[derive(Debug, Clone, BorshSerialize, BorshDeserialize)]
pub struct Chunks {
    /// The bitfields tracking chunk state.
    bits: Vec<u8>,
    /// The tracking capacity which is
    /// ```rust
    /// let capacity = bits.len() * BIT_FIELD_SIZE
    /// ```
    /// The amount of tracked chunks could be a bit smaller as it might only use
    /// part of the last bit in [Chunks::bits].
    /// This count gives that smaller amount.
    count: usize,
    /// The size of chunks that we are tracking.
    chunk_size: u16,
}

impl fmt::Display for Chunks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (idx, bit) in self.bits.iter().enumerate() {
            if idx % 8 == 0 {
                write!(f, "\n{:05}: ", idx * BIT_FIELD_SIZE)?;
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

impl Chunks {
    pub fn new(chunk_count: usize, chunk_size: u16) -> Self {
        // SAFETY: this is a bug and we need to crash and burn
        assert!(
            Self::bytes_for_count_len(chunk_count)
                < consts::MAX_ACOUNT_ALLOC_PER_INSTRUCTION_SIZE as usize,
            "Size ({}) needed to track {} chunks is too large track and would require to realloc. Max allowed is {} bytes",
            Self::bytes_for_count_len(chunk_count),
            chunk_count,
            consts::MAX_ACOUNT_ALLOC_PER_INSTRUCTION_SIZE
        );
        Self {
            bits: vec![0; Self::bits_for_count_len(chunk_count)],
            count: chunk_count,
            chunk_size,
        }
    }

    fn bits_for_count_len(count: usize) -> usize {
        count / BIT_FIELD_SIZE + 1
    }

    pub fn bytes_for_count_len(count: usize) -> usize {
        // bits: Vec<u8>,
        Self::bits_for_count_len(count) * std::mem::size_of::<u8>()
        // count: usize,
        + std::mem::size_of::<usize>()
        // chunk_size: u16,
        + std::mem::size_of::<u16>()
    }

    /// Returns `true` if the chunk at index has been delivered
    pub fn get_idx(&self, idx: usize) -> bool {
        if idx >= self.count {
            return false;
        }
        let vec_idx = idx / BIT_FIELD_SIZE;
        let bit_idx = idx % BIT_FIELD_SIZE;
        (self.bits[vec_idx] & (1 << bit_idx)) != 0
    }

    /// Sets the chunk at index to `true` denoting that it has been delivered
    pub(super) fn set_idx(&mut self, idx: usize) {
        if idx < self.count {
            let vec_idx = idx / BIT_FIELD_SIZE;
            let bit_idx = idx % BIT_FIELD_SIZE;
            self.bits[vec_idx] |= 1 << bit_idx;
        }
    }

    pub fn set_offset(&mut self, offset: usize) -> CommittorResult<()> {
        if offset % self.chunk_size as usize != 0 {
            return Err(CommittorError::OffsetMustBeMultipleOfChunkSize(
                offset,
                self.chunk_size,
            ));
        }
        let idx = offset / self.chunk_size as usize;
        self.set_idx(idx);
        Ok(())
    }

    pub fn get_offset(&self, offset: usize) -> CommittorResult<bool> {
        if offset % self.chunk_size as usize != 0 {
            return Err(CommittorError::OffsetMustBeMultipleOfChunkSize(
                offset,
                self.chunk_size,
            ));
        }
        let idx = offset / self.chunk_size as usize;
        Ok(self.get_idx(idx))
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn chunk_size(&self) -> u16 {
        self.chunk_size
    }

    pub fn get_missing_chunks(&self) -> HashSet<usize> {
        (0..self.count).filter(|&i| !self.get_idx(i)).collect()
    }

    pub fn is_complete(&self) -> bool {
        self.get_missing_chunks().is_empty()
    }
}

impl From<(Vec<bool>, u16)> for Chunks {
    fn from((vec, chunk_size): (Vec<bool>, u16)) -> Self {
        let bits = vec![0; vec.len() / BIT_FIELD_SIZE + 1];
        let mut chunks = Self {
            bits,
            count: vec.len(),
            chunk_size,
        };
        for (i, &d) in vec.iter().enumerate() {
            if d {
                chunks.set_idx(i);
            }
        }
        chunks
    }
}

#[cfg(test)]
mod test {
    use super::*;

    impl Chunks {
        pub(super) fn iter(&self) -> ChunksIter {
            ChunksIter {
                chunks: self,
                idx: 0,
            }
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
                Some(self.chunks.get_idx(idx))
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
        chunks.set_idx(0);
        chunks.set_idx(10);

        assert!(chunks.get_idx(0));
        assert!(!chunks.get_idx(1));
        assert!(chunks.get_idx(10));

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
    fn test_chunks_set_get_idx_large() {
        let chunks = vec![false; 2048];
        let mut chunks = Chunks::from((chunks, CHUNK_SIZE));
        chunks.set_idx(99);
        chunks.set_idx(1043);

        assert!(!chunks.get_idx(0));
        assert!(!chunks.get_idx(1));
        assert!(chunks.get_idx(99));
        assert!(!chunks.get_idx(1042));
        assert!(chunks.get_idx(1043));
        assert!(!chunks.get_idx(1044));

        assert!(!chunks.get_idx(2048));
        assert!(!chunks.get_idx(2049));

        assert_eq!(chunks.iter().count(), 2048);
    }
}
