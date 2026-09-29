pub use rocksdb::Direction as IteratorDirection;

pub enum IteratorMode<Index> {
    Start,
    End,
    From(Index, IteratorDirection),
}
