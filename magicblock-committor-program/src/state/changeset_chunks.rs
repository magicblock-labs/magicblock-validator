use std::collections::HashSet;

use borsh::{BorshDeserialize, BorshSerialize};

use super::chunks::Chunks;

/// A chunk of change set data that we want to apply to the on chain
/// [ChangeSet] buffer
#[derive(Debug, Default, BorshSerialize, BorshDeserialize)]
pub struct ChangesetChunk {
    // u32 is sufficient since the buffer size is limited and we will
    // never exceed the u32 max value with an offset we need to address
    // u32 max:      4_294_967_295
    // max offset      ~10_000_660
    pub offset: u32,
    pub data_chunk: Vec<u8>,
    // chunk size can never exceed the ix max size which is well below u16::MAX (65_535)
    #[borsh_skip]
    chunk_size: u16,
}

impl From<(&[u8], u32, u16)> for ChangesetChunk {
    fn from((data, offset, chunk_size): (&[u8], u32, u16)) -> Self {
        let end = {
            let end = (offset + chunk_size as u32) as usize;
            // For the last chunk we might have less data than the chunk size left
            end.min(data.len())
        };
        Self {
            offset,
            data_chunk: data[offset as usize..end].to_vec(),
            chunk_size,
        }
    }
}

impl ChangesetChunk {
    /// The index that the chunk will have in the [Chunks] tracker.
    pub fn chunk_idx(&self) -> u32 {
        self.offset / self.chunk_size as u32
    }
}

/// This is a helper struct which is never stored anywhere, but merely
/// combines the [Chunks] and [ChangeSetChunks::chunk_size] in order
/// to provide convenience methods.
pub struct ChangesetChunks<'chunks> {
    /// The size of each data chunk that we send to fill the buffer.
    /// It is a u16 since u16 max (65,535) is much larger than the max packet size (1,280)
    chunk_size: u16,
    /// Keeping track of which chunks have been delivered
    chunks: &'chunks Chunks,
}

impl<'chunks> ChangesetChunks<'chunks> {
    pub fn new(chunks: &'chunks Chunks, chunk_size: u16) -> Self {
        Self { chunks, chunk_size }
    }

    fn assert_sizes(&self, data: &[u8]) {
        let chunks_len = self.chunks.count() * self.chunk_size as usize;
        assert!(
            data.len() < chunks_len,
            "data.len() ({}) >= chunks_len ({})",
            data.len(),
            chunks_len
        );
        assert!(
            chunks_len < data.len() + self.chunk_size as usize,
            "chunks_len ({}) >= data.len() + chunk_size ({})",
            chunks_len,
            data.len() + self.chunk_size as usize
        );
    }

    pub fn iter<'data>(
        &'chunks self,
        data: &'data [u8],
    ) -> ChangesetChunksIter<'data> {
        self.assert_sizes(data);
        ChangesetChunksIter::new(
            data,
            self.chunk_size,
            self.chunks.count(),
            None,
        )
    }

    pub fn iter_missing<'data>(
        &self,
        data: &'data [u8],
    ) -> ChangesetChunksIter<'data> {
        self.assert_sizes(data);
        ChangesetChunksIter::new(
            data,
            self.chunk_size,
            self.chunks.count(),
            Some(self.chunks.get_missing_chunks()),
        )
    }
}

pub struct ChangesetChunksIter<'data> {
    /// The data from which to extract chunks
    data: &'data [u8],
    /// Size of each chunk
    chunk_size: u16,
    /// Total number of chunks in the data
    chunk_count: usize,
    /// If set, only include chunks that are in the filter
    filter: Option<HashSet<usize>>,
    /// Current index of the iterator
    idx: usize,
}

impl<'data> ChangesetChunksIter<'data> {
    pub fn new(
        data: &'data [u8],
        chunk_size: u16,
        chunk_count: usize,
        filter: Option<HashSet<usize>>,
    ) -> Self {
        Self {
            data,
            chunk_size,
            chunk_count,
            filter,
            idx: 0,
        }
    }
}

impl Iterator for ChangesetChunksIter<'_> {
    type Item = ChangesetChunk;

    fn next(&mut self) -> Option<Self::Item> {
        // Skip all chunks that are not in the filter
        if let Some(filter) = &self.filter {
            while self.idx < self.chunk_count {
                if filter.contains(&self.idx) {
                    break;
                }
                self.idx += 1;
            }
        }

        if self.idx >= self.chunk_count {
            return None;
        }

        let offset = self.idx * self.chunk_size as usize;
        assert!(
            offset < self.data.len(),
            "offset out of bounds {} >= {}",
            offset,
            self.data.len()
        );

        let chunk =
            ChangesetChunk::from((self.data, offset as u32, self.chunk_size));

        self.idx += 1;

        Some(chunk)
    }
}
