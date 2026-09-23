//! Bookkeeping that keeps the inline sugar passes linear.
//!
//! Each pass walks a line looking for an opener, then walks on for its closer.
//! A line of openers with no closer (`||a ||a ||a …`) would repeat that second
//! walk to the end of the line once per opener, quadratic in a line an author
//! controls entirely. Everything here answers in constant time what a walk
//! would have found.

/// Running totals over one text run, so "does this span hold any" is two
/// lookups instead of a walk over the span.
pub(crate) struct Counts(Vec<u32>);

impl Counts {
    pub(crate) fn new(len: usize, hit: impl Fn(usize) -> bool) -> Self {
        let mut totals = Vec::with_capacity(len + 1);
        let mut n = 0u32;
        totals.push(n);
        for i in 0..len {
            n += u32::from(hit(i));
            totals.push(n);
        }
        Self(totals)
    }

    /// Whether any position in `start..end` is a hit.
    pub(crate) fn any(&self, start: usize, end: usize) -> bool {
        start < end && self.0[end] > self.0[start]
    }
}

/// The last closer search of one pass.
///
/// Searches only move forward, and every one starts on a position the
/// previous walk stepped on: never inside a tag it skipped, never on the
/// second character of a pair it jumped. So a search that starts between the
/// previous start and where that walk stopped takes the same steps from there
/// and reaches the same answer. It gets that answer without walking again.
#[derive(Default)]
pub(crate) struct Closers {
    last: Option<(usize, usize, Option<usize>)>,
}

impl Closers {
    /// `scan` returns the closer, if any, and the position its walk stopped.
    pub(crate) fn find(
        &mut self,
        from: usize,
        scan: impl FnOnce(usize) -> (Option<usize>, usize),
    ) -> Option<usize> {
        if let Some((start, stop, found)) = self.last
            && (start..=stop).contains(&from)
        {
            return found;
        }
        let (found, stop) = scan(from);
        self.last = Some((from, stop, found));
        found
    }
}
