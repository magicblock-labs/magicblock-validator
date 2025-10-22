use core::slice;
use std::marker::PhantomData;

pub enum SizeChanged {
    Expanded(usize),
    Shrunk(usize),
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OffsetPair {
    pub offset_in_diff: u32,
    pub offset_in_data: u32,
}

pub struct DiffSet<'a> {
    buf: *const u8,
    len: usize,
    _marker: PhantomData<&'a [u8]>,
}

impl<'a> DiffSet<'a> {
    pub fn new(diff: &[u8]) -> Self {
        Self {
            buf: diff.as_ptr(),
            len: diff.len(),
            _marker: PhantomData,
        }
    }

    pub fn num_offset_pairs(&self) -> usize {
        unsafe { *(self.buf as *const u32) as usize }
    }

    pub fn offset_pairs(&self) -> &'a [OffsetPair] {
        unsafe {
            let pairs_ptr = self.buf.add(4);
            slice::from_raw_parts(
                pairs_ptr as *const OffsetPair,
                self.num_offset_pairs(),
            )
        }
    }

    ///
    /// Returns a diff-slice at the given index and also returns the offset-in-account-data
    /// where the returned diff-slice must be applied.
    ///
    pub fn diff_slice_at(&self, index: usize) -> Option<(&'a [u8], usize)> {
        let num_slices = self.num_offset_pairs();
        if index >= num_slices {
            return None;
        }
        let offsets = self.offset_pairs();
        let current_offset = offsets[index];
        let slice_len = {
            if index + 1 < num_slices {
                offsets[index + 1].offset_in_diff
                    - current_offset.offset_in_diff
            } else {
                self.concatenated_diff_slice_len() as u32
                    - current_offset.offset_in_diff
            }
        };
        Some((
            unsafe {
                slice::from_raw_parts(
                    self.concatenated_diff_slice_begin()
                        .add(current_offset.offset_in_diff as usize),
                    slice_len as usize,
                )
            },
            current_offset.offset_in_data as usize,
        ))
    }

    fn concatenated_diff_slice_begin(&self) -> *const u8 {
        unsafe { self.buf.add(4).add(self.num_offset_pairs() * 8) }
    }

    fn concatenated_diff_slice_len(&self) -> usize {
        self.len - (4 + self.num_offset_pairs() * 8)
    }
}
