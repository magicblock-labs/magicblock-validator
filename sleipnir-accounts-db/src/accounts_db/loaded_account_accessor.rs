use std::borrow::Cow;

use crate::accounts_cache::CachedAccount;

#[derive(Debug)]
pub enum LoadedAccountAccessor<'a> {
    // NOTE: we don't flush yet and thus also don't have the Stored variant here
    /// None value in Cached variant means the cache was flushed
    Cached(Option<Cow<'a, CachedAccount>>),
}
