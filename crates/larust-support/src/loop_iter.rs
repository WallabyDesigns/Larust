//! Laravel Blade's automatic `$loop` variable (`$loop->first`, `->last`,
//! `->index`, `->iteration`, `->count`) - as an explicit iterator adapter
//! a template opts into, not implicit per-`@foreach` magic. Laravel
//! injects `$loop` into *every* `@foreach` body automatically; Larust's
//! own `@foreach` (`larust-view`/`larust-macros`) doesn't special-case
//! anything at all for this - `@foreach((item, loop_) in
//! items.iter().with_loop())` is ordinary tuple-binding codegen (already
//! built for keyed iteration, see `larust_view::ast::Node::Foreach`'s doc
//! comment) paired with an ordinary iterator combinator, so no framework
//! change was needed to add this at all, only this module plus
//! `larust-convert`'s own wiring (`blade::scan::body_references_loop_variable`).
//!
//! Named `loop_`, not `loop` - `loop` is a reserved Rust keyword, so no
//! template can bind a variable literally called `loop`.
//!
//! Deliberately a narrower field set than Laravel's real `$loop`:
//! `remaining`, `even`/`odd`, `depth`, and `parent` (nested-loop access)
//! aren't included - add them if a real conversion ever needs them,
//! rather than guessing at a full port nobody's asked for yet.

/// One iteration's position, alongside the item itself - see [`WithLoop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loop {
    /// 0-based position - Rust's own `Iterator::enumerate()` convention.
    pub index: usize,
    /// 1-based position - Laravel's own `$loop->iteration`.
    pub iteration: usize,
    /// Total number of items this loop will produce.
    pub count: usize,
    pub first: bool,
    pub last: bool,
}

/// `.with_loop()` - wraps any iterator whose length is known up front
/// (`ExactSizeIterator`, satisfied by `.iter()` on a `Vec`/slice/
/// `HashMap`, `.iter().enumerate()` over one of those, and more) into one
/// yielding `(item, Loop)` pairs.
pub trait WithLoop: ExactSizeIterator + Sized {
    fn with_loop(self) -> LoopIter<Self> {
        LoopIter {
            count: self.len(),
            index: 0,
            inner: self,
        }
    }
}

impl<I: ExactSizeIterator> WithLoop for I {}

pub struct LoopIter<I> {
    inner: I,
    index: usize,
    count: usize,
}

impl<I: ExactSizeIterator> Iterator for LoopIter<I> {
    type Item = (I::Item, Loop);

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.inner.next()?;
        let index = self.index;
        self.index += 1;
        Some((
            item,
            Loop {
                index,
                iteration: index + 1,
                count: self.count,
                first: index == 0,
                last: index + 1 == self.count,
            },
        ))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<I: ExactSizeIterator> ExactSizeIterator for LoopIter<I> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_the_first_and_last_item_and_counts_correctly() {
        let items = ["a", "b", "c"];
        let collected: Vec<_> = items.iter().with_loop().collect();
        assert_eq!(collected.len(), 3);

        let (_, first_loop) = collected[0];
        assert_eq!(first_loop.index, 0);
        assert_eq!(first_loop.iteration, 1);
        assert_eq!(first_loop.count, 3);
        assert!(first_loop.first);
        assert!(!first_loop.last);

        let (_, last_loop) = collected[2];
        assert_eq!(last_loop.index, 2);
        assert_eq!(last_loop.iteration, 3);
        assert!(!last_loop.first);
        assert!(last_loop.last);
    }

    #[test]
    fn a_single_item_is_both_first_and_last() {
        let items = ["only"];
        let collected: Vec<_> = items.iter().with_loop().collect();
        assert_eq!(collected.len(), 1);
        let (_, meta) = collected[0];
        assert!(meta.first);
        assert!(meta.last);
    }

    #[test]
    fn composes_with_an_already_enumerated_iterator() {
        // The exact real shape a keyed `@foreach` translates to:
        // `.iter().enumerate().with_loop()`.
        let items = ["a", "b"];
        let collected: Vec<_> = items.iter().enumerate().with_loop().collect();
        assert_eq!(collected.len(), 2);
        let ((index, _value), meta) = collected[1];
        assert_eq!(index, 1);
        assert!(meta.last);
    }
}
