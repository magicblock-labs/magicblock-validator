use std::{
    mem::{align_of, size_of},
    slice,
};

use borsh::{BorshDeserialize, BorshSerialize};
use static_assertions::const_assert;

#[repr(C)]
#[derive(
    BorshSerialize, BorshDeserialize, Debug, Clone, Copy, Default, PartialEq, Eq,
)]
pub struct OrderLevel {
    pub price: u64, // ideally both fields could be some decimal value
    pub size: u64,
}

const_assert!(align_of::<OrderLevel>() == align_of::<u64>());
const_assert!(size_of::<OrderLevel>() == 16);

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, Default)]
pub struct BookUpdate {
    pub bids: Vec<OrderLevel>,
    pub asks: Vec<OrderLevel>,
}

#[repr(C)]
pub struct OrderBookHeader {
    pub bids_len: u32,
    pub asks_len: u32,
}

const_assert!(align_of::<OrderBookHeader>() == align_of::<u32>());
const_assert!(size_of::<OrderBookHeader>() == 8);

const ORDER_LEVEL_SIZE: usize = std::mem::size_of::<OrderLevel>();
const HEADER_SIZE: usize = std::mem::size_of::<OrderBookHeader>();

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct OrderBookOwned {
    pub bids: Vec<OrderLevel>,
    pub asks: Vec<OrderLevel>,
}

impl From<&OrderBook<'_>> for OrderBookOwned {
    fn from(book_ref: &OrderBook) -> Self {
        let mut book = Self::default();
        book.bids.extend_from_slice(book_ref.bids());
        book.asks.extend(book_ref.asks_reversed().iter().rev());
        book
    }
}

impl borsh::de::BorshDeserialize for OrderBookOwned {
    fn deserialize(buf: &mut &[u8]) -> Result<Self, borsh::io::Error> {
        let (book_bytes, rest) = buf.split_at(buf.len());
        *buf = rest; // rest is actually empty

        // I make a copy so that I can get mutable bytes in the unsafe block below.
        // I could take mutable bytes from &[u8] as well and unsafe block will not
        // stop me, but that would break aliasing rules and therefore would invoke UB.
        // It's a test code, so copying should be OK.
        let book_bytes = {
            let mut aligned = rkyv::AlignedVec::with_capacity(book_bytes.len());
            aligned.extend_from_slice(book_bytes);
            aligned
        };

        Ok(Self::from(&OrderBook::new(unsafe {
            slice::from_raw_parts_mut(
                book_bytes.as_ptr() as *mut u8,
                book_bytes.len(),
            )
        })))
    }
    fn deserialize_reader<R: borsh::io::Read>(
        _reader: &mut R,
    ) -> ::core::result::Result<Self, borsh::io::Error> {
        unimplemented!("deserialize_reader() not implemented. Please use buffer version as it needs to know size of the buffer")
    }
}
pub struct OrderBook<'a> {
    header: &'a mut OrderBookHeader,
    capacity: usize,
    levels: *mut OrderLevel,
}

impl<'a> OrderBook<'a> {
    //
    // ========= Zero-Copy Order Book ==========
    //
    // -----------------------------------------
    // |          account data                 |
    // -----------------------------------------
    // | header |          levels              |
    // -----------------------------------------
    //          | asks grows ->  <- bids grows |
    // -----------------------------------------
    //
    // Note:
    //
    //  - asks grows towards right
    //  - bids grows towards left
    //
    pub fn new(data: &'a mut [u8]) -> Self {
        let (header_bytes, levels_bytes) = data.split_at_mut(HEADER_SIZE);

        assert!(
            header_bytes
                .as_ptr()
                .align_offset(align_of::<OrderBookHeader>())
                == 0
                && levels_bytes.as_ptr().align_offset(align_of::<OrderLevel>())
                    == 0,
            "data is not properly aligned for OrderBook to be constructed"
        );

        Self {
            header: unsafe {
                &mut *(header_bytes.as_ptr() as *mut OrderBookHeader)
            },
            capacity: levels_bytes.len() / ORDER_LEVEL_SIZE,
            levels: levels_bytes.as_mut_ptr() as *mut OrderLevel,
        }
    }

    pub fn update_from(&mut self, updates: BookUpdate) {
        self.add_bids(&updates.bids);
        self.add_asks(&updates.asks);
    }

    pub fn add_bids(
        &mut self,
        bids: &[OrderLevel],
    ) -> Option<&'a [OrderLevel]> {
        if self.remaining_capacity() < bids.len() {
            return None;
        }
        let new_bids_len = self.bids_len() + bids.len();
        let bids_space =
            unsafe { self.bids_with_uninitialized_slots(new_bids_len) };

        bids_space[self.bids_len()..].copy_from_slice(bids);
        self.header.bids_len = new_bids_len as u32;

        Some(bids_space)
    }

    pub fn add_asks(
        &mut self,
        asks: &[OrderLevel],
    ) -> Option<&'a [OrderLevel]> {
        if self.remaining_capacity() < asks.len() {
            return None;
        }
        let new_asks_len = self.asks_len() + asks.len();
        let asks_space =
            unsafe { self.asks_with_uninitialized_slots(new_asks_len) };

        // copy in the reverse order
        for (dst, src) in
            asks_space[..asks.len()].iter_mut().zip(asks.iter().rev())
        {
            *dst = *src;
        }
        self.header.asks_len = new_asks_len as u32;

        Some(asks_space)
    }

    pub fn bids(&self) -> &'a [OrderLevel] {
        unsafe { slice::from_raw_parts(self.levels, self.bids_len()) }
    }

    /// Note that the returned slice is in reverse order, means the first entry is the latest
    /// entry and the last entry is the oldest entry.
    pub fn asks_reversed(&self) -> &'a [OrderLevel] {
        unsafe {
            slice::from_raw_parts(
                self.levels.add(self.capacity - self.asks_len()),
                self.asks_len(),
            )
        }
    }

    pub fn bids_len(&self) -> usize {
        self.header.bids_len as usize
    }

    pub fn asks_len(&self) -> usize {
        self.header.asks_len as usize
    }

    unsafe fn bids_with_uninitialized_slots(
        &mut self,
        bids_len: usize,
    ) -> &'a mut [OrderLevel] {
        slice::from_raw_parts_mut(self.levels, bids_len)
    }

    unsafe fn asks_with_uninitialized_slots(
        &mut self,
        asks_len: usize,
    ) -> &'a mut [OrderLevel] {
        slice::from_raw_parts_mut(
            self.levels.add(self.capacity - asks_len),
            asks_len,
        )
    }

    fn remaining_capacity(&self) -> usize {
        self.capacity
            .checked_sub((self.header.bids_len + self.header.asks_len) as usize)
            .expect("remaining_capacity must exist")
    }
}
