// Providing pieces of delegation SDK until it is sufficiently granular for us to use
// exactly the pieces we need from a common crate.

// -----------------
// CommitAccountArgs
// -----------------
pub(crate) struct CommitAccountArgs {
    pub(crate) slot: u64,
    pub(crate) allow_undelegation: bool,
    pub(crate) data: Vec<u8>,
}

impl CommitAccountArgs {
    const SIZE_WITHOUT_DATA: usize = 8 + 1;
    const SIZE_DATA_VEC_LEN: usize = 4;

    /// Serializes the commit account args into a byte vector.
    /// We use a manual implementation to avoid having to pull in the entire
    /// delegation program SDK and/or borsh.
    pub(crate) fn to_vec(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(
            Self::SIZE_WITHOUT_DATA + Self::SIZE_DATA_VEC_LEN + self.data.len(),
        );
        data.extend_from_slice(&self.slot.to_le_bytes());
        data.push(self.allow_undelegation as u8);
        data.extend_from_slice(&(self.data.len() as u32).to_le_bytes());
        data.extend_from_slice(&self.data);
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_commit_account_args_to_vec() {
        {
            let args = CommitAccountArgs {
                slot: 100,
                allow_undelegation: true,
                data: vec![0, 1, 2, 9, 9, 9, 6, 7, 8, 9],
            };
            let expected = vec![
                100, 0, 0, 0, 0, 0, 0, 0, // slot
                1, // allow_undelegation
                10, 0, 0, 0, // data.len
                0, 1, 2, 9, 9, 9, 6, 7, 8, 9, // data
            ];
            assert_eq!(args.to_vec(), expected);
        }

        {
            let args = CommitAccountArgs {
                slot: 999888777,
                allow_undelegation: true,
                data: vec![0, 1, 2, 9, 9, 9, 6, 7, 8, 9],
            };
            let expected = vec![
                137, 23, 153, 59, 0, 0, 0, 0, // slot
                1, // allow_undelegation
                10, 0, 0, 0, // data.len
                0, 1, 2, 9, 9, 9, 6, 7, 8, 9, // data
            ];
            assert_eq!(args.to_vec(), expected);
        }

        {
            let args = CommitAccountArgs {
                slot: 100,
                allow_undelegation: true,
                data: vec![0, 1, 2, 3],
            };
            let expected = vec![
                100, 0, 0, 0, 0, 0, 0, 0, // slot
                1, // allow_undelegation
                4, 0, 0, 0, // data.len
                0, 1, 2, 3, // data
            ];
            assert_eq!(args.to_vec(), expected);
        }

        {
            let args = CommitAccountArgs {
                slot: 100,
                allow_undelegation: false,
                data: vec![0, 1, 2, 9, 9, 9, 6, 7, 8, 9],
            };
            let expected = vec![
                100, 0, 0, 0, 0, 0, 0, 0, // slot
                0, // allow_undelegation
                10, 0, 0, 0, // data.len
                0, 1, 2, 9, 9, 9, 6, 7, 8, 9, // data
            ];
            assert_eq!(args.to_vec(), expected);
        }
    }
}
