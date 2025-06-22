use borsh::{to_vec, BorshDeserialize};
use magicblock_committor_program::{
    instruction::{
        create_init_ix, create_realloc_buffer_ixs, CreateInitIxArgs,
        CreateReallocBufferIxArgs,
    },
    instruction_chunks::chunk_realloc_ixs,
    ChangedAccount, Changeset, Chunks,
};
use solana_program_test::*;
use solana_pubkey::Pubkey;
use solana_sdk::{
    blake3::HASH_BYTES, hash::Hash, native_token::LAMPORTS_PER_SOL,
    signer::Signer, transaction::Transaction,
};

macro_rules! exec {
    ($banks_client:ident, $ix:expr, $auth:ident, $latest_blockhash:ident) => {{
        let mut transaction =
            Transaction::new_with_payer($ix, Some(&$auth.pubkey()));
        transaction.sign(&[$auth.insecure_clone()], $latest_blockhash);
        $banks_client
            .process_transaction(transaction)
            .await
            .unwrap();
    }};
}

macro_rules! get_chunks {
    ($banks_client:expr, $chunks_pda:expr) => {{
        let chunks_data = $banks_client
            .get_account($chunks_pda)
            .await
            .unwrap()
            .unwrap()
            .data;
        Chunks::try_from_slice(&chunks_data).unwrap()
    }};
}

macro_rules! get_buffer_data {
    ($banks_client:expr, $buffer_pda:expr) => {{
        $banks_client
            .get_account($buffer_pda)
            .await
            .unwrap()
            .unwrap()
            .data
    }};
}

#[tokio::test]
async fn test_init_write_and_close_small_single_account() {
    let mut changeset = Changeset::default();
    changeset.add(
        Pubkey::new_unique(),
        ChangedAccount::Full {
            owner: Pubkey::new_unique(),
            lamports: LAMPORTS_PER_SOL,
            data: vec![1; 500],
            bundle_id: 1,
        },
    );
    init_write_and_close(changeset).await;
}

const MULTIPLE_ITER: u64 = 3;

#[tokio::test]
async fn test_init_write_and_close_small_changeset() {
    let mut changeset = Changeset::default();
    for i in 1..MULTIPLE_ITER {
        changeset.add(
            Pubkey::new_unique(),
            ChangedAccount::Full {
                owner: Pubkey::new_unique(),
                lamports: i,
                data: vec![i as u8; 500],
                bundle_id: 1,
            },
        );
    }
    init_write_and_close(changeset).await;
}

#[tokio::test]
async fn test_init_write_and_close_large_changeset() {
    let mut changeset = Changeset::default();
    for i in 1..MULTIPLE_ITER {
        let pubkey = Pubkey::new_unique();
        changeset.add(
            pubkey,
            ChangedAccount::Full {
                owner: Pubkey::new_unique(),
                lamports: 1000 + i,
                data: vec![i as u8; i as usize * 500],
                bundle_id: 1,
            },
        );
        if i % 2 == 0 {
            changeset.request_undelegation(pubkey)
        }
    }
    init_write_and_close(changeset).await;
}

#[tokio::test]
async fn test_init_write_and_close_very_large_changeset() {
    let mut changeset = Changeset::default();
    for i in 1..MULTIPLE_ITER {
        let pubkey = Pubkey::new_unique();
        changeset.add(
            pubkey,
            ChangedAccount::Full {
                owner: Pubkey::new_unique(),
                lamports: 1000 + i,
                data: vec![i as u8; i as usize * 5_000],
                bundle_id: 1,
            },
        );
        if i % 2 == 0 {
            changeset.request_undelegation(pubkey)
        }
    }
    init_write_and_close(changeset).await;
}

#[tokio::test]
async fn test_init_write_and_close_extremely_large_changeset() {
    let mut changeset = Changeset::default();
    for i in 1..MULTIPLE_ITER {
        let pubkey = Pubkey::new_unique();
        changeset.add(
            pubkey,
            ChangedAccount::Full {
                owner: Pubkey::new_unique(),
                lamports: 1000 + i,
                data: vec![i as u8; i as usize * 50_000],
                bundle_id: 1,
            },
        );
        if i % 2 == 0 {
            changeset.request_undelegation(pubkey)
        }
    }
    init_write_and_close(changeset).await;
}

#[tokio::test]
async fn test_init_write_and_close_insanely_large_changeset() {
    let mut changeset = Changeset::default();
    for i in 1..MULTIPLE_ITER {
        let pubkey = Pubkey::new_unique();
        changeset.add(
            pubkey,
            ChangedAccount::Full {
                owner: Pubkey::new_unique(),
                lamports: 1000 + i,
                data: vec![i as u8; i as usize * 90_000],
                bundle_id: 1,
            },
        );
        if i % 2 == 0 {
            changeset.request_undelegation(pubkey)
        }
    }
    init_write_and_close(changeset).await;
}

async fn init_write_and_close(changeset: Changeset) {
    let program_id = &magicblock_committor_program::id();

    let (banks_client, auth, _) = ProgramTest::new(
        "committor_program",
        *program_id,
        processor!(magicblock_committor_program::process),
    )
    .start()
    .await;

    let ephem_blockhash = Hash::from([1; HASH_BYTES]);

    let chunk_size = 439 / 14;
    let commitables = changeset.into_committables(chunk_size);
    for commitable in commitables.iter() {
        let chunks =
            Chunks::new(commitable.chunk_count(), commitable.chunk_size());

        // Initialize the Changeset on chain
        let (chunks_pda, buffer_pda) = {
            let chunks_account_size = to_vec(&chunks).unwrap().len() as u64;
            let (init_ix, chunks_pda, buffer_pda) =
                create_init_ix(CreateInitIxArgs {
                    authority: auth.pubkey(),
                    pubkey: commitable.pubkey,
                    chunks_account_size,
                    buffer_account_size: commitable.size() as u64,
                    blockhash: ephem_blockhash,
                    chunk_count: commitable.chunk_count(),
                    chunk_size: commitable.chunk_size(),
                });
            let realloc_ixs =
                create_realloc_buffer_ixs(CreateReallocBufferIxArgs {
                    authority: auth.pubkey(),
                    pubkey: commitable.pubkey,
                    buffer_account_size: commitable.size() as u64,
                    blockhash: ephem_blockhash,
                });

            let ix_chunks = chunk_realloc_ixs(realloc_ixs, Some(init_ix));
            for ixs in ix_chunks {
                let latest_blockhash =
                    banks_client.get_latest_blockhash().await.unwrap();
                exec!(banks_client, &ixs, auth, latest_blockhash);
            }

            (chunks_pda, buffer_pda)
        };

        let chunks = get_chunks!(&banks_client, chunks_pda);
        for i in 0..chunks.count() {
            assert!(!chunks.get_idx(i));
        }
        assert!(!chunks.is_complete());

        let latest_blockhash =
            banks_client.get_latest_blockhash().await.unwrap();

        // Write the first chunk
        {
            let first_chunk = &commitable.iter_all().next().unwrap();
            let write_ix = magicblock_committor_program::instruction::create_write_ix(
                magicblock_committor_program::instruction::CreateWriteIxArgs {
                    authority: auth.pubkey(),
                    pubkey: commitable.pubkey,
                    offset: first_chunk.offset,
                    data_chunk: first_chunk.data_chunk.clone(),
                    blockhash: ephem_blockhash,
                },
            );
            exec!(banks_client, &[write_ix], auth, latest_blockhash);

            let chunks = get_chunks!(&banks_client, chunks_pda);
            assert_eq!(chunks.count(), commitable.chunk_count());
            assert_eq!(chunks.chunk_size(), commitable.chunk_size());
            assert!(chunks.get_idx(0));
            for i in 1..chunks.count() {
                assert!(!chunks.get_idx(i));
            }
            assert!(!chunks.is_complete());

            let buffer_data = get_buffer_data!(&banks_client, buffer_pda);
            assert_eq!(
                buffer_data[0..first_chunk.data_chunk.len()],
                first_chunk.data_chunk
            );
        }

        // Write third chunk
        {
            let third_chunk = &commitable.iter_all().nth(2).unwrap();
            let write_ix = magicblock_committor_program::instruction::create_write_ix(
                magicblock_committor_program::instruction::CreateWriteIxArgs {
                    authority: auth.pubkey(),
                    pubkey: commitable.pubkey,
                    offset: third_chunk.offset,
                    data_chunk: third_chunk.data_chunk.clone(),
                    blockhash: ephem_blockhash,
                },
            );
            exec!(banks_client, &[write_ix], auth, latest_blockhash);

            let chunks = get_chunks!(&banks_client, chunks_pda);
            assert!(chunks.get_idx(0));
            assert!(!chunks.get_idx(1));
            assert!(chunks.get_idx(2));
            for i in 3..chunks.count() {
                assert!(!chunks.get_idx(i));
            }
            assert!(!chunks.is_complete());

            let buffer_data = get_buffer_data!(&banks_client, buffer_pda);
            assert_eq!(
                buffer_data[third_chunk.offset as usize
                    ..third_chunk.offset as usize
                        + third_chunk.data_chunk.len()],
                third_chunk.data_chunk
            );
        }

        // Write the remaining chunks
        {
            for chunk in commitable.iter_missing() {
                let latest_blockhash =
                    banks_client.get_latest_blockhash().await.unwrap();
                let write_ix = magicblock_committor_program::instruction::create_write_ix(
                    magicblock_committor_program::instruction::CreateWriteIxArgs {
                        authority: auth.pubkey(),
                        pubkey: commitable.pubkey,
                        offset: chunk.offset,
                        data_chunk: chunk.data_chunk.clone(),
                        blockhash: ephem_blockhash,
                    },
                );
                exec!(banks_client, &[write_ix], auth, latest_blockhash);
            }

            let chunks = get_chunks!(&banks_client, chunks_pda);
            for i in 0..chunks.count() {
                assert!(chunks.get_idx(i));
            }
            assert!(chunks.is_complete());

            let buffer = get_buffer_data!(&banks_client, buffer_pda);
            assert_eq!(buffer, commitable.data);
        }

        // Close both accounts
        {
            let latest_blockhash =
                banks_client.get_latest_blockhash().await.unwrap();

            // Normally this instruction would be part of a transaction that processes
            // the change set to update the corresponding accounts
            let close_ix = magicblock_committor_program::instruction::create_close_ix(
                magicblock_committor_program::instruction::CreateCloseIxArgs {
                    authority: auth.pubkey(),
                    pubkey: commitable.pubkey,
                    blockhash: ephem_blockhash,
                },
            );
            exec!(banks_client, &[close_ix], auth, latest_blockhash);

            assert!(banks_client
                .get_account(chunks_pda)
                .await
                .unwrap()
                .is_none());
            assert!(banks_client
                .get_account(buffer_pda)
                .await
                .unwrap()
                .is_none());
        }
    }
}
