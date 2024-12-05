use std::borrow::Cow;

use crate::accounts_cache::CachedAccount;

pub enum LoadedAccount<'a> {
    Cached(Cow<'a, CachedAccount>),
    // NOTE: not yet supporting stored account meta
}
