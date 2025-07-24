pub trait Merge: Default {
    fn merge(&mut self, other: Self);
}
