use solana_account_info::AccountInfo;
use solana_instruction::AccountMeta;

pub(crate) fn remaining_to_metas<'a>(
    remaining: &[(AccountInfo<'a>, bool, bool)],
) -> Vec<AccountMeta> {
    remaining
        .iter()
        .map(|(ai, is_signer, is_writable)| AccountMeta {
            pubkey: *ai.key,
            is_signer: *is_signer,
            is_writable: *is_writable,
        })
        .collect()
}

pub(crate) fn collect_account_infos<'a>(
    prefix: &[AccountInfo<'a>],
    remaining: &[(AccountInfo<'a>, bool, bool)],
) -> Vec<AccountInfo<'a>> {
    let mut out = prefix.to_vec();
    out.extend(remaining.iter().map(|(ai, _, _)| ai.clone()));
    out
}
