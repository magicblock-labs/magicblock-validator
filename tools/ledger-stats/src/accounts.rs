use magicblock_accounts_db::account_storage::meta::StoredAccountMeta;
use magicblock_accounts_db::AccountsPersister;
use magicblock_ledger::Ledger;
use num_format::{Locale, ToFormattedString};
use solana_sdk::account::ReadableAccount;
use solana_sdk::clock::Epoch;
use solana_sdk::pubkey::Pubkey;
use tabular::{Row, Table};

struct AccountInfo<'a> {
    /// Pubkey of the account
    pub pubkey: &'a Pubkey,
    /// lamports in the account
    pub lamports: u64,
    /// the epoch at which this account will next owe rent
    pub rent_epoch: Epoch,
    /// the program that owns this account. If executable, the program that loads this account.
    pub owner: &'a Pubkey,
    /// this account's data contains a loaded program (and is now read-only)
    pub executable: bool,
    /// the data in this account
    pub data: &'a [u8],
}

pub fn print_accounts(ledger: &Ledger, include_rent_epoch: bool) {
    let accounts_dir = ledger
        .ledger_path()
        .parent()
        .expect("Ledger path has no parent")
        .join("accounts")
        .join("run");
    let persister = AccountsPersister::new_with_paths(vec![accounts_dir]);
    let storage = persister.read_most_recent_store().unwrap();

    let table_alignment = if include_rent_epoch {
        "{:<}  {:<}  {:>}  {:<}  {:>}  {:>}"
    } else {
        "{:<}  {:<}  {:>}  {:<}  {:>}"
    };
    let mut table = Table::new(table_alignment);
    let mut row = Row::new()
        .with_cell("Pubkey")
        .with_cell("Owner")
        .with_cell("Lamports")
        .with_cell("Executable")
        .with_cell("Data Length");
    if include_rent_epoch {
        row.add_cell("Rent Epoch");
    }
    table.add_row(row);

    fn add_row(table: &mut Table, meta: AccountInfo, include_rent_epoch: bool) {
        let mut row = Row::new()
            .with_cell(meta.pubkey.to_string())
            .with_cell(meta.owner.to_string())
            .with_cell(meta.lamports.to_formatted_string(&Locale::en))
            .with_cell(meta.executable)
            .with_cell(meta.data.len());
        if include_rent_epoch {
            row.add_cell(meta.rent_epoch.to_formatted_string(&Locale::en));
        }
        table.add_row(row);
    }

    for acc in storage.all_accounts() {
        match acc {
            StoredAccountMeta::AppendVec(acc) => add_row(
                &mut table,
                AccountInfo {
                    pubkey: acc.pubkey(),
                    lamports: acc.lamports(),
                    rent_epoch: acc.rent_epoch(),
                    owner: acc.owner(),
                    executable: acc.executable(),
                    data: acc.data(),
                },
                include_rent_epoch,
            ),
            StoredAccountMeta::Hot(acc) => add_row(
                &mut table,
                AccountInfo {
                    pubkey: acc.address(),
                    lamports: acc.lamports(),
                    rent_epoch: acc.rent_epoch(),
                    owner: acc.owner(),
                    executable: acc.executable(),
                    data: acc.data(),
                },
                include_rent_epoch,
            ),
        };
    }

    println!("{}", table);
}
