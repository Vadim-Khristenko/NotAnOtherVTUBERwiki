//! Line diff of two page bodies, laid out old beside new, with the changed
//! words inside a changed line marked.
//!
//! A Markdown paragraph is one line, so a line diff alone shows a one word
//! edit as a whole paragraph removed and another added. Removed and added
//! lines that resemble each other are paired, and each pair is diffed again
//! by words.

use similar::algorithms::{Capture, diff_deadline};
use similar::{Algorithm, ChangeTag, DiffOp, capture_diff_deadline, capture_diff_slices_deadline};
use std::time::{Duration, Instant};

/// Unchanged lines kept either side of a change.
const CONTEXT: usize = 3;

/// Most rows in one diff; past this it is truncated and says so.
pub const ROW_MAX: usize = 1500;

/// Search deadline on a page anyone can open; past it the diff is correct
/// but coarser.
const TIME_MAX: Duration = Duration::from_millis(250);

/// Longest body, in lines, whose diff is compacted. Compaction ignores the
/// deadline and is quadratic on long repetitive bodies.
const COMPACT_LINES_MAX: usize = 2000;

/// Most similarity checks in one block of removed and added lines. A bigger
/// block is paired in order.
const PAIR_CHECKS_MAX: usize = 64;

/// Longest line, in tokens, that gets words marked.
const INLINE_TOKENS_MAX: usize = 20_000;

/// A removed and an added line with at least this share of tokens in common
/// are the same line, edited.
const SAME_LINE_RATIO: f32 = 0.5;

/// A run of text on one side of a changed line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    /// Removed on the old side, added on the new side.
    pub changed: bool,
}

/// One side of a changed row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Side {
    /// 1-based line number on this side.
    pub line: usize,
    pub segments: Vec<Segment>,
}

#[cfg(test)]
impl Side {
    pub fn text(&self) -> String {
        self.segments.iter().map(|s| s.text.as_str()).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// The same line on both sides.
    Context {
        old_line: usize,
        new_line: usize,
        text: String,
    },
    /// A removed line, an added line, or both when one became the other.
    Change {
        old: Option<Side>,
        new: Option<Side>,
        /// The first row of a run of changes, numbered from 1.
        hunk: Option<usize>,
    },
    /// A collapsed run of unchanged lines.
    Gap { hidden: usize },
}

impl Row {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Context { .. } => "ctx",
            Self::Change { .. } => "chg",
            Self::Gap { .. } => "gap",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Diff {
    pub rows: Vec<Row>,
    pub added: usize,
    pub removed: usize,
    /// Runs of changes, counted before truncation.
    pub hunks: usize,
    /// `ROW_MAX` cut the rows short.
    pub truncated: bool,
}

pub fn diff_bodies(old: &str, new: &str) -> Diff {
    // Lines keep their terminator, so a last line with and without a newline differ.
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let deadline = Instant::now() + TIME_MAX;
    let ops = line_ops(&old_lines, &new_lines, deadline);

    let mut rows = Vec::new();
    let mut block = Block::default();
    let (mut added, mut removed) = (0, 0);
    for op in &ops {
        for change in op.iter_changes(&old_lines, &new_lines) {
            let text = strip_newline(change.value());
            match change.tag() {
                ChangeTag::Equal => {
                    block.flush(&mut rows, deadline);
                    rows.push(Row::Context {
                        old_line: change.old_index().map_or(0, |i| i + 1),
                        new_line: change.new_index().map_or(0, |i| i + 1),
                        text: text.to_string(),
                    });
                }
                ChangeTag::Delete => {
                    removed += 1;
                    block.removed.push((change.old_index().map_or(0, |i| i + 1), text));
                }
                ChangeTag::Insert => {
                    added += 1;
                    block.added.push((change.new_index().map_or(0, |i| i + 1), text));
                }
            }
        }
    }
    block.flush(&mut rows, deadline);

    let mut rows = collapse(rows);
    let hunks = number_hunks(&mut rows);
    let truncated = rows.len() > ROW_MAX;
    rows.truncate(ROW_MAX);
    Diff {
        rows,
        added,
        removed,
        hunks,
        truncated,
    }
}

fn line_ops(old: &[&str], new: &[&str], deadline: Instant) -> Vec<DiffOp> {
    if old.len().max(new.len()) <= COMPACT_LINES_MAX {
        return capture_diff_deadline(
            Algorithm::Myers,
            old,
            0..old.len(),
            new,
            0..new.len(),
            Some(deadline),
        );
    }
    let mut capture = Capture::new();
    let Ok(()) = diff_deadline(
        Algorithm::Myers,
        &mut capture,
        old,
        0..old.len(),
        new,
        0..new.len(),
        Some(deadline),
    );
    capture.into_ops()
}

fn strip_newline(line: &str) -> &str {
    line.trim_end_matches(['\n', '\r'])
}

/// Removed and added lines between two unchanged ones.
#[derive(Default)]
struct Block<'a> {
    removed: Vec<(usize, &'a str)>,
    added: Vec<(usize, &'a str)>,
}

impl Block<'_> {
    fn flush(&mut self, rows: &mut Vec<Row>, deadline: Instant) {
        if self.removed.is_empty() && self.added.is_empty() {
            return;
        }
        let matches = self.matches(deadline);
        let (mut next_old, mut next_new) = (0, 0);
        for (old_at, new_at) in matches
            .iter()
            .copied()
            .chain(std::iter::once((self.removed.len(), self.added.len())))
        {
            // Lines between two matches are shown side by side in order, unmarked.
            let olds = &self.removed[next_old..old_at];
            let news = &self.added[next_new..new_at];
            for i in 0..olds.len().max(news.len()) {
                rows.push(Row::Change {
                    old: olds.get(i).map(|&(line, text)| plain(line, text)),
                    new: news.get(i).map(|&(line, text)| plain(line, text)),
                    hunk: None,
                });
            }
            if let (Some(&(old_line, old_text)), Some(&(new_line, new_text))) =
                (self.removed.get(old_at), self.added.get(new_at))
            {
                let (old_segments, new_segments) = mark_words(old_text, new_text, deadline);
                rows.push(Row::Change {
                    old: Some(Side {
                        line: old_line,
                        segments: old_segments,
                    }),
                    new: Some(Side {
                        line: new_line,
                        segments: new_segments,
                    }),
                    hunk: None,
                });
            }
            next_old = old_at + 1;
            next_new = new_at + 1;
        }
        self.removed.clear();
        self.added.clear();
    }

    /// Pairs of (removed, added) indices that are one line edited, in order.
    fn matches(&self, deadline: Instant) -> Vec<(usize, usize)> {
        let (olds, news) = (&self.removed, &self.added);
        if olds.is_empty() || news.is_empty() {
            return Vec::new();
        }
        // One line became one line: that is an edit however much changed.
        if olds.len() == 1 && news.len() == 1 {
            return vec![(0, 0)];
        }
        let mut out = Vec::new();
        let mut checks = 0;
        let mut from = 0;
        for (i, &(_, old_text)) in olds.iter().enumerate() {
            for (j, &(_, new_text)) in news.iter().enumerate().skip(from) {
                if checks >= PAIR_CHECKS_MAX || Instant::now() >= deadline {
                    return out;
                }
                checks += 1;
                if similarity(old_text, new_text, deadline) >= SAME_LINE_RATIO {
                    out.push((i, j));
                    from = j + 1;
                    break;
                }
            }
        }
        out
    }
}

fn plain(line: usize, text: &str) -> Side {
    Side {
        line,
        segments: vec![Segment {
            text: text.to_string(),
            changed: false,
        }],
    }
}

/// Words, runs of spaces, and single other characters. Punctuation stands
/// alone, so a changed comma does not mark the word before it.
fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut class = None;
    for (at, c) in text.char_indices() {
        let this = if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        };
        if class != Some(this) || this == 2 {
            if at > start {
                out.push(&text[start..at]);
            }
            start = at;
        }
        class = Some(this);
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

fn similarity(old: &str, new: &str, deadline: Instant) -> f32 {
    let (a, b) = (tokens(old), tokens(new));
    if a.len() > INLINE_TOKENS_MAX || b.len() > INLINE_TOKENS_MAX {
        return 0.0;
    }
    let ops = capture_diff_slices_deadline(Algorithm::Myers, &a, &b, Some(deadline));
    similar::diff_ratio(&ops, a.len(), b.len())
}

/// The two sides of an edited line, with the removed and added words marked.
fn mark_words(old: &str, new: &str, deadline: Instant) -> (Vec<Segment>, Vec<Segment>) {
    let (a, b) = (tokens(old), tokens(new));
    if a.len() > INLINE_TOKENS_MAX || b.len() > INLINE_TOKENS_MAX || Instant::now() >= deadline {
        return (plain(0, old).segments, plain(0, new).segments);
    }
    let ops = capture_diff_slices_deadline(Algorithm::Myers, &a, &b, Some(deadline));
    let (mut left, mut right) = (Vec::new(), Vec::new());
    for op in &ops {
        for change in op.iter_changes(&a, &b) {
            let piece = (change.tag() != ChangeTag::Equal, change.value());
            match change.tag() {
                ChangeTag::Equal => {
                    left.push(piece);
                    right.push(piece);
                }
                ChangeTag::Delete => left.push(piece),
                ChangeTag::Insert => right.push(piece),
            }
        }
    }
    (join(left), join(right))
}

/// Merges neighbouring pieces, and marks a space between two marked pieces so
/// a changed phrase reads as one mark instead of a row of them.
fn join(pieces: Vec<(bool, &str)>) -> Vec<Segment> {
    let mut flags: Vec<bool> = pieces.iter().map(|p| p.0).collect();
    for i in 1..pieces.len().saturating_sub(1) {
        if !flags[i] && flags[i - 1] && flags[i + 1] && pieces[i].1.trim().is_empty() {
            flags[i] = true;
        }
    }
    let mut out: Vec<Segment> = Vec::new();
    for (i, (_, text)) in pieces.into_iter().enumerate() {
        match out.last_mut() {
            Some(last) if last.changed == flags[i] => last.text.push_str(text),
            _ => out.push(Segment {
                text: text.to_string(),
                changed: flags[i],
            }),
        }
    }
    out
}

/// Replaces runs of unchanged lines with one `Gap` row, when the run is
/// long enough that hiding it saves more than a few lines.
fn collapse(rows: Vec<Row>) -> Vec<Row> {
    // A gap row standing in for fewer lines than this is not worth the click.
    let keep = CONTEXT * 2 + 3;
    let is_context = |row: &Row| matches!(row, Row::Context { .. });
    let mut out = Vec::with_capacity(rows.len());
    let mut index = 0;
    while index < rows.len() {
        if !is_context(&rows[index]) {
            out.push(rows[index].clone());
            index += 1;
            continue;
        }
        let start = index;
        while index < rows.len() && is_context(&rows[index]) {
            index += 1;
        }
        let run = &rows[start..index];
        if run.len() <= keep {
            out.extend_from_slice(run);
            continue;
        }
        // A run at the start or end has one inner edge to keep context for.
        let at_start = start == 0;
        let at_end = index == rows.len();
        if !at_start {
            out.extend_from_slice(&run[..CONTEXT]);
        }
        let hidden =
            run.len() - if at_start { 0 } else { CONTEXT } - if at_end { 0 } else { CONTEXT };
        out.push(Row::Gap { hidden });
        if !at_end {
            out.extend_from_slice(&run[run.len() - CONTEXT..]);
        }
    }
    out
}

fn number_hunks(rows: &mut [Row]) -> usize {
    let mut count = 0;
    let mut in_hunk = false;
    for row in rows.iter_mut() {
        match row {
            Row::Change { hunk, .. } => {
                if !in_hunk {
                    count += 1;
                    *hunk = Some(count);
                }
                in_hunk = true;
            }
            _ => in_hunk = false,
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(diff: &Diff) -> Vec<&'static str> {
        diff.rows.iter().map(Row::kind).collect()
    }

    fn changes(diff: &Diff) -> Vec<(Option<Side>, Option<Side>)> {
        diff.rows
            .iter()
            .filter_map(|r| match r {
                Row::Change { old, new, .. } => Some((old.clone(), new.clone())),
                _ => None,
            })
            .collect()
    }

    fn marked(side: &Side) -> Vec<&str> {
        side.segments
            .iter()
            .filter(|s| s.changed)
            .map(|s| s.text.as_str())
            .collect()
    }

    #[test]
    fn an_identical_body_has_no_changes() {
        let diff = diff_bodies("a\nb\nc\n", "a\nb\nc\n");
        assert_eq!((diff.added, diff.removed, diff.hunks), (0, 0, 0));
        assert!(!diff.truncated);
    }

    #[test]
    fn a_changed_line_is_one_row_with_both_sides() {
        let diff = diff_bodies("a\nb\nc\n", "a\nB\nc\n");
        assert_eq!((diff.added, diff.removed), (1, 1));
        assert_eq!(kinds(&diff), vec!["ctx", "chg", "ctx"]);
        let (old, new) = &changes(&diff)[0];
        assert_eq!(old.as_ref().map(|s| s.line), Some(2));
        assert_eq!(new.as_ref().map(|s| s.line), Some(2));
    }

    #[test]
    fn only_the_changed_words_are_marked() {
        let diff = diff_bodies(
            "A long line with many words about Filian.\n",
            "A long line with some words about Filian and snackers.\n",
        );
        let (old, new) = &changes(&diff)[0];
        let (old, new) = (old.as_ref().unwrap(), new.as_ref().unwrap());
        assert_eq!(marked(old), vec!["many"]);
        assert_eq!(marked(new), vec!["some", " and snackers"]);
        assert_eq!(old.text(), "A long line with many words about Filian.");
        assert_eq!(new.text(), "A long line with some words about Filian and snackers.");
    }

    #[test]
    fn a_space_between_two_changed_words_joins_them() {
        let diff = diff_bodies("keep one two keep\n", "keep uno dos keep\n");
        let (old, _) = &changes(&diff)[0];
        assert_eq!(marked(old.as_ref().unwrap()), vec!["one two"]);
    }

    #[test]
    fn punctuation_changes_do_not_mark_the_neighbouring_word() {
        let diff = diff_bodies("Hello, world\n", "Hello; world\n");
        let (old, new) = &changes(&diff)[0];
        assert_eq!(marked(old.as_ref().unwrap()), vec![","]);
        assert_eq!(marked(new.as_ref().unwrap()), vec![";"]);
    }

    #[test]
    fn an_added_line_has_no_old_side() {
        let diff = diff_bodies("a\n", "a\nb\n");
        let (old, new) = &changes(&diff)[0];
        assert!(old.is_none());
        assert_eq!(new.as_ref().map(|s| s.line), Some(2));
        assert!(new.as_ref().unwrap().segments.iter().all(|s| !s.changed));
    }

    #[test]
    fn a_line_inserted_before_an_edited_one_is_paired_correctly() {
        let diff = diff_bodies(
            "intro\nthe quick brown fox jumps\nend\n",
            "intro\na brand new line here\nthe quick brown cat jumps\nend\n",
        );
        let rows = changes(&diff);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].0.is_none(), "the new line stands alone");
        let (old, new) = (&rows[1].0, &rows[1].1);
        assert_eq!(marked(old.as_ref().unwrap()), vec!["fox"]);
        assert_eq!(marked(new.as_ref().unwrap()), vec!["cat"]);
    }

    #[test]
    fn separate_runs_of_changes_are_numbered() {
        let old: String = (0..30).map(|i| format!("line {i}\n")).collect();
        let new = old.replace("line 3\n", "LINE 3\n").replace("line 25\n", "LINE 25\n");
        let diff = diff_bodies(&old, &new);
        assert_eq!(diff.hunks, 2);
        let hunks: Vec<usize> = diff
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Change { hunk, .. } => *hunk,
                _ => None,
            })
            .collect();
        assert_eq!(hunks, vec![1, 2]);
    }

    #[test]
    fn trailing_newlines_do_not_reach_the_row_text() {
        let diff = diff_bodies("one\r\n", "two\r\n");
        let (old, new) = &changes(&diff)[0];
        assert!(!old.as_ref().unwrap().text().contains(['\n', '\r']));
        assert!(!new.as_ref().unwrap().text().contains(['\n', '\r']));
    }

    #[test]
    fn a_long_unchanged_run_collapses_to_one_gap() {
        let old: String = (0..60).map(|i| format!("line {i}\n")).collect();
        let new = old.replace("line 30", "LINE 30");
        let diff = diff_bodies(&old, &new);
        // Line 30 replaced: 27 hidden above (30 minus 3 of context), 26 below.
        let hidden: Vec<usize> = diff
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Gap { hidden } => Some(*hidden),
                _ => None,
            })
            .collect();
        assert_eq!(hidden, vec![27, 26]);
        let shown = diff.rows.iter().filter(|r| r.kind() == "ctx").count();
        assert_eq!(shown + 27 + 26, 59);
    }

    #[test]
    fn a_short_run_is_left_alone_rather_than_swapped_for_a_marker() {
        let old = format!(
            "X\n{}Y\n",
            (0..7).map(|i| format!("c{i}\n")).collect::<String>()
        );
        let new = old.replace("X\n", "x\n").replace("Y\n", "y\n");
        assert!(!kinds(&diff_bodies(&old, &new)).contains(&"gap"));
    }

    #[test]
    fn a_run_at_the_start_of_the_file_keeps_no_leading_context() {
        let old: String = (0..40).map(|i| format!("line {i}\n")).collect();
        let new = format!("{old}tail\n");
        let diff = diff_bodies(&old, &new);
        assert_eq!(diff.rows.first().map(Row::kind), Some("gap"));
        assert_eq!((diff.added, diff.removed), (1, 0));
    }

    #[test]
    fn an_enormous_diff_is_cut_and_admits_it() {
        let old: String = (0..4000).map(|i| format!("old {i}\n")).collect();
        let new: String = (0..4000).map(|i| format!("new {i}\n")).collect();
        let diff = diff_bodies(&old, &new);
        assert!(diff.truncated);
        assert_eq!(diff.rows.len(), ROW_MAX);
        assert_eq!((diff.added, diff.removed), (4000, 4000));
    }

    #[test]
    fn a_long_repetitive_diff_stays_cheap() {
        let old: String = (0..60_000).map(|i| format!("{}\n", i % 3)).collect();
        let new: String = (0..60_000).map(|i| format!("{}\n", (i + 1) % 2)).collect();
        let started = Instant::now();
        let diff = diff_bodies(&old, &new);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(diff.truncated);
        assert!(diff.added > 0 && diff.removed > 0);
    }

    #[test]
    fn one_enormous_edited_line_stays_cheap() {
        let old: String = (0..200_000).map(|i| format!("w{} ", i % 7)).collect();
        let new: String = (0..200_000).map(|i| format!("w{} ", i % 5)).collect();
        let started = Instant::now();
        let diff = diff_bodies(&old, &new);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!((diff.added, diff.removed), (1, 1));
    }

    #[test]
    fn diffing_against_an_empty_body_is_all_additions() {
        let diff = diff_bodies("", "a\nb\n");
        assert_eq!((diff.added, diff.removed), (2, 0));
    }

    #[test]
    fn cyrillic_words_are_marked_whole() {
        let diff = diff_bodies("Филиан любит снекерсов\n", "Филиан обожает снекерсов\n");
        let (old, new) = &changes(&diff)[0];
        assert_eq!(marked(old.as_ref().unwrap()), vec!["любит"]);
        assert_eq!(marked(new.as_ref().unwrap()), vec!["обожает"]);
    }
}
