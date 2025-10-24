use std::slice;

use borsh::BorshDeserialize;

#[repr(C)]
#[derive(Clone, Copy, BorshDeserialize)]
pub struct OrderLevel {
    pub price: f64,
    pub size: f64,
    pub timestamp: u64,
}

#[repr(C, align(4))]
pub struct OrderBookHeader {
    pub bids_len: u32,
    pub asks_len: u32,
}

const ORDER_LEVEL_SIZE: usize = std::mem::size_of::<OrderLevel>();
const HEADER_SIZE: usize = std::mem::size_of::<OrderBookHeader>();

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
        Self {
            header: unsafe {
                &mut *(header_bytes.as_ptr() as *mut OrderBookHeader)
            },
            capacity: levels_bytes.len() / ORDER_LEVEL_SIZE,
            levels: levels_bytes.as_mut_ptr() as *mut OrderLevel,
        }
    }

    pub fn add_bids(
        &mut self,
        bids: &[OrderLevel],
    ) -> Option<&'a [OrderLevel]> {
        if self.remaining_capacity() < bids.len() {
            return None;
        }
        let new_bids_len = self.bids_len() + bids.len();
        let bids_space = unsafe { self.bids(new_bids_len) };

        bids_space[self.bids_len()..].copy_from_slice(bids);
        self.header.bids_len = new_bids_len as u32;

        Some(bids_space)
    }

    pub fn bids_len(&self) -> usize {
        self.header.bids_len as usize
    }

    pub fn add_asks(
        &mut self,
        asks: &[OrderLevel],
    ) -> Option<&'a [OrderLevel]> {
        if self.remaining_capacity() < asks.len() {
            return None;
        }
        let new_asks_len = self.asks_len() + asks.len();
        let asks_space = unsafe { self.asks(new_asks_len) };

        // copy in the reverse order
        for (dst, src) in
            asks_space[..asks.len()].iter_mut().zip(asks.iter().rev())
        {
            *dst = *src;
        }
        self.header.asks_len = new_asks_len as u32;

        Some(asks_space)
    }

    pub fn asks_len(&self) -> usize {
        self.header.asks_len as usize
    }

    unsafe fn bids(&mut self, bids_len: usize) -> &'a mut [OrderLevel] {
        slice::from_raw_parts_mut(self.levels, bids_len)
    }

    unsafe fn asks(&mut self, asks_len: usize) -> &'a mut [OrderLevel] {
        slice::from_raw_parts_mut(
            self.levels.add(self.capacity - asks_len),
            asks_len as usize,
        )
    }

    fn remaining_capacity(&self) -> usize {
        self.capacity
            .checked_sub((self.header.bids_len + self.header.asks_len) as usize)
            .expect("remaining_capacity must exist")
    }
}
