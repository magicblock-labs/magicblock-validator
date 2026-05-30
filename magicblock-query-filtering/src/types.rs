use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;
use thiserror::Error;

use crate::PERMISSION_PROGRAM_ID;

const PERMISSION_PREFIX: &[u8] = b"permission:";
const PERMISSION_HEADER_LEN: usize = 34;
const MEMBER_COUNT_LEN: usize = 4;

const MEMBER_FLAG_TX_LOGS: u8 = 1 << 1;
const MEMBER_FLAG_TX_BALANCES: u8 = 1 << 2;
const MEMBER_FLAG_TX_MESSAGE: u8 = 1 << 3;
const MEMBER_FLAG_ACCOUNT_SIGNATURE: u8 = 1 << 4;

pub type PermissionResult<T> = Result<T, PermissionError>;

#[derive(Debug, Error)]
pub enum PermissionError {
    #[error(
        "permission data too short: len={len}, expected at least {min_len}"
    )]
    DataTooShort { len: usize, min_len: usize },
    #[error("invalid permission option tag: {tag}")]
    InvalidOptionTag { tag: u8 },
    #[error("invalid permission member bytes: len={len}")]
    InvalidMemberBytes { len: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub flags: u8,
    pub pubkey: Pubkey,
}

impl Member {
    pub const SIZE: usize = 33;

    pub fn can_see_tx_logs(&self) -> bool {
        self.flags & MEMBER_FLAG_TX_LOGS != 0
    }

    pub fn can_see_tx_balances(&self) -> bool {
        self.flags & MEMBER_FLAG_TX_BALANCES != 0
    }

    pub fn can_see_tx_message(&self) -> bool {
        self.flags & MEMBER_FLAG_TX_MESSAGE != 0
    }

    pub fn can_see_account_signature(&self) -> bool {
        self.flags & MEMBER_FLAG_ACCOUNT_SIGNATURE != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permission {
    discriminator: u8,
    bump: u8,
    permissioned_account: Pubkey,
    members: Option<Vec<Member>>,
}

impl Permission {
    pub fn pda(account: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(
            &[PERMISSION_PREFIX, account.as_ref()],
            &PERMISSION_PROGRAM_ID,
        )
        .0
    }

    pub fn decode(data: &[u8]) -> PermissionResult<Self> {
        if data.len() < PERMISSION_HEADER_LEN {
            return Err(PermissionError::DataTooShort {
                len: data.len(),
                min_len: PERMISSION_HEADER_LEN,
            });
        }

        let discriminator = data[0];
        let bump = data[1];
        let permissioned_account = Pubkey::new_from_array(
            data[2..PERMISSION_HEADER_LEN].try_into().map_err(|_| {
                PermissionError::DataTooShort {
                    len: data.len(),
                    min_len: PERMISSION_HEADER_LEN,
                }
            })?,
        );
        let members =
            Self::decode_members_payload(&data[PERMISSION_HEADER_LEN..])?;

        Ok(Permission {
            discriminator,
            bump,
            permissioned_account,
            members,
        })
    }

    fn decode_members_payload(
        payload: &[u8],
    ) -> PermissionResult<Option<Vec<Member>>> {
        if let Some(members) = Self::decode_counted_members(payload) {
            let members = members?;
            return Ok((!members.is_empty()).then_some(members));
        }

        let option_tag =
            *payload.first().ok_or(PermissionError::DataTooShort {
                len: PERMISSION_HEADER_LEN,
                min_len: PERMISSION_HEADER_LEN + 1,
            })?;
        match option_tag {
            0 => Ok(None),
            1 => Ok(Some(Self::decode_private_members(&payload[1..])?)),
            _ => Err(PermissionError::InvalidOptionTag { tag: option_tag }),
        }
    }

    fn decode_private_members(payload: &[u8]) -> PermissionResult<Vec<Member>> {
        if let Some(members) = Self::decode_counted_members(payload) {
            return members;
        }

        // Ephemeral permission accounts omit the member count and may include
        // account padding after the serialized members.
        let members_len = payload.len() - (payload.len() % Member::SIZE);
        Self::decode_members(&payload[..members_len])
    }

    fn decode_counted_members(
        payload: &[u8],
    ) -> Option<PermissionResult<Vec<Member>>> {
        let len_bytes: [u8; MEMBER_COUNT_LEN] =
            payload.get(..MEMBER_COUNT_LEN)?.try_into().ok()?;
        let count = u32::from_le_bytes(len_bytes) as usize;
        let members_len = count.checked_mul(Member::SIZE)?;
        let members_end = MEMBER_COUNT_LEN.checked_add(members_len)?;
        let members_data = payload.get(MEMBER_COUNT_LEN..members_end)?;
        let trailing_padding = payload
            .get(members_end..)
            .map(|tail| tail.iter().all(|byte| *byte == 0))
            .unwrap_or(true);

        trailing_padding.then(|| Self::decode_members(members_data))
    }

    fn decode_members(members_data: &[u8]) -> PermissionResult<Vec<Member>> {
        if !members_data.len().is_multiple_of(Member::SIZE) {
            return Err(PermissionError::InvalidMemberBytes {
                len: members_data.len(),
            });
        }

        members_data
            .chunks_exact(Member::SIZE)
            .map(|chunk| {
                let pubkey = Pubkey::new_from_array(
                    chunk[1..Member::SIZE].try_into().map_err(|_| {
                        PermissionError::InvalidMemberBytes { len: chunk.len() }
                    })?,
                );
                Ok(Member {
                    flags: chunk[0],
                    pubkey,
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub fn new(
        permissioned_account: Pubkey,
        members: Option<Vec<Member>>,
    ) -> Self {
        Self {
            discriminator: 0,
            bump: 255,
            permissioned_account,
            members,
        }
    }

    #[cfg(test)]
    pub fn encode(&self, ephemeral: bool) -> Vec<u8> {
        let mut data = Vec::new();
        data.push(0); // discriminator
        data.push(255); // bump
        data.extend_from_slice(self.permissioned_account.as_ref()); // permissioned account
        match &self.members {
            Some(members) => {
                data.push(1);
                if !ephemeral {
                    data.extend_from_slice(
                        &(members.len() as u32).to_le_bytes(),
                    );
                }
                for member in members {
                    data.push(member.flags);
                    data.extend_from_slice(member.pubkey.as_ref());
                }
            }
            None => data.push(0),
        }
        data
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestrictedEntry {
    pub members: Vec<Member>,
    pub are_program_restricted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access {
    pub account: bool,
    pub logs: bool,
    pub message: bool,
    pub balances: bool,
    pub signatures: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionEntry {
    Unrestricted,
    Restricted(RestrictedEntry),
}

impl PermissionEntry {
    pub fn from_permission(permission: Permission) -> Self {
        permission
            .members
            .map(|members| {
                Self::Restricted(RestrictedEntry {
                    members,
                    are_program_restricted: true,
                })
            })
            .unwrap_or(Self::Unrestricted)
    }

    pub fn access_for(&self, user: &Pubkey) -> Access {
        match self {
            Self::Unrestricted => Access {
                account: true,
                logs: true,
                message: true,
                balances: true,
                signatures: true,
            },
            Self::Restricted(entry) => entry
                .members
                .iter()
                .find(|member| member.pubkey == *user)
                .map(|member| Access {
                    account: true,
                    logs: member.can_see_tx_logs(),
                    message: member.can_see_tx_message(),
                    balances: member.can_see_tx_balances(),
                    signatures: member.can_see_account_signature(),
                })
                .unwrap_or(Access {
                    account: false,
                    logs: false,
                    message: false,
                    balances: false,
                    signatures: false,
                }),
        }
    }

    pub fn is_program_restricted(&self, program: &Pubkey) -> bool {
        match self {
            Self::Unrestricted => false,
            Self::Restricted(entry) => {
                entry.are_program_restricted
                    && !entry
                        .members
                        .iter()
                        .any(|member| member.pubkey == *program)
            }
        }
    }
}

// ------------------------------
// Responses
// ------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct ChallengeResponse {
    pub challenge: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub pubkey: String,
    pub signature: String,
    pub challenge: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QuoteResponse {
    pub quote: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FastQuoteResponse {
    pub quote: String,
    pub pubkey: String,
    pub challenge: String,
    pub signature: String,
    pub report_data_sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_is_encoded_correctly() {
        let original_permissions = vec![
            Permission::new(Pubkey::new_unique(), None),
            Permission::new(
                Pubkey::new_unique(),
                Some(vec![Member {
                    pubkey: Pubkey::new_unique(),
                    flags: u8::MAX,
                }]),
            ),
            Permission::new(
                Pubkey::new_unique(),
                Some(vec![
                    Member {
                        pubkey: Pubkey::new_unique(),
                        flags: u8::MAX,
                    };
                    10
                ]),
            ),
            Permission::new(
                Pubkey::new_unique(),
                Some(vec![Member {
                    pubkey: Pubkey::new_unique(),
                    flags: u8::MAX,
                }]),
            ),
        ];

        for ephemeral in [true, false] {
            for original_permission in &original_permissions {
                let permission_data = original_permission.encode(ephemeral);
                let permission = Permission::decode(&permission_data).unwrap();
                assert_eq!(permission, *original_permission);
            }
        }
    }

    #[test]
    fn permission_entry_is_encoded_correctly() {
        let original_permission_entries = vec![
            (
                Permission::new(Pubkey::new_unique(), None),
                PermissionEntry::Unrestricted,
            ),
            {
                let permissioned_account = Pubkey::new_unique();
                let member_pk = Pubkey::new_unique();
                (
                    Permission::new(
                        permissioned_account,
                        Some(vec![Member {
                            pubkey: member_pk,
                            flags: u8::MAX,
                        }]),
                    ),
                    PermissionEntry::Restricted(RestrictedEntry {
                        members: vec![Member {
                            pubkey: member_pk,
                            flags: u8::MAX,
                        }],
                        are_program_restricted: true,
                    }),
                )
            },
        ];

        for (original_permission, original_permission_entry) in
            original_permission_entries
        {
            let permission_entry =
                PermissionEntry::from_permission(original_permission);
            assert_eq!(permission_entry, original_permission_entry);
        }
    }

    #[test]
    fn missing_permission_is_unrestricted() {
        let account = Pubkey::new_unique();

        let permission = PermissionEntry::from_permission(
            Permission::decode(&Permission::new(account, None).encode(false))
                .unwrap(),
        );

        assert_eq!(permission, PermissionEntry::Unrestricted);
    }

    #[test]
    fn restricted_permission_controls_access() {
        let account = Pubkey::new_unique();
        let allowed = Pubkey::new_unique();
        let denied = Pubkey::new_unique();

        let permission_data = Permission {
            discriminator: 0,
            bump: 255,
            permissioned_account: account,
            members: Some(vec![Member {
                pubkey: allowed,
                flags: u8::MAX,
            }]),
        }
        .encode(false);

        let permission = PermissionEntry::from_permission(
            Permission::decode(&permission_data).unwrap(),
        );

        assert!(permission.access_for(&allowed).account);
        assert!(!permission.access_for(&denied).account);
    }

    #[test]
    fn padded_ephemeral_permission_controls_access() {
        let account = Pubkey::new_unique();
        let default_member = Pubkey::new_unique();
        let allowed = Pubkey::new_unique();
        let denied = Pubkey::new_unique();

        let mut permission_data = Permission {
            discriminator: 0,
            bump: 255,
            permissioned_account: account,
            members: Some(vec![
                Member {
                    pubkey: default_member,
                    flags: 0,
                },
                Member {
                    pubkey: allowed,
                    flags: u8::MAX,
                },
            ]),
        }
        .encode(true);
        permission_data.extend_from_slice(&[0; 7]);

        let permission = PermissionEntry::from_permission(
            Permission::decode(&permission_data).unwrap(),
        );

        assert!(permission.access_for(&allowed).account);
        assert!(!permission.access_for(&denied).account);
    }

    #[test]
    fn count_prefixed_permission_controls_access() {
        let account = Pubkey::new_unique();
        let allowed = Pubkey::new_unique();
        let denied = Pubkey::new_unique();
        let members = [Member {
            pubkey: allowed,
            flags: u8::MAX,
        }];

        let mut permission_data = Vec::new();
        permission_data.push(0);
        permission_data.push(255);
        permission_data.extend_from_slice(account.as_ref());
        permission_data
            .extend_from_slice(&(members.len() as u32).to_le_bytes());
        for member in members {
            permission_data.push(member.flags);
            permission_data.extend_from_slice(member.pubkey.as_ref());
        }

        let permission = PermissionEntry::from_permission(
            Permission::decode(&permission_data).unwrap(),
        );

        assert!(permission.access_for(&allowed).account);
        assert!(!permission.access_for(&denied).account);
    }
}
