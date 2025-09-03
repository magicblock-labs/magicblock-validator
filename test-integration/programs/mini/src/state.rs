pub const COUNTER_SIZE: usize = 8; // size of u64

#[derive(Debug, Clone, PartialEq)]
pub struct Counter {
    pub count: u64,
}

impl Counter {
    pub fn new() -> Self {
        Self { count: 0 }
    }

    pub fn increment(&mut self) {
        self.count = self.count.saturating_add(1);
    }

    pub fn to_bytes(&self) -> [u8; 8] {
        self.count.to_le_bytes()
    }

    pub fn from_bytes(
        data: &[u8],
    ) -> Result<Self, std::array::TryFromSliceError> {
        let count = u64::from_le_bytes(data[..8].try_into()?);
        Ok(Self { count })
    }
}

impl Default for Counter {
    fn default() -> Self {
        Self::new()
    }
}
