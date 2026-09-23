//! Bookkeeping that keeps the inline sugar passes linear.
//!
//! A line of openers without closers (`||a ||a ...`) would otherwise walk to
//! the line end once per opener.

/// Running totals over one text run, so a span check is two lookups.
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
/// Searches only move forward and start on a position the previous walk
/// stepped on, so one starting inside the previous walk's range reaches the
/// same answer, which is returned without walking again.
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
