//! A small, owned interface over the diff pipeline.
//!
//! These types deliberately don't expose difftastic's internal
//! representation (`MatchedPos`, `Syntax`, ...), so the internals can
//! be refactored without breaking library users.

use std::ffi::OsStr;

use crate::display::context::all_matched_lines_filled;
use crate::driver::diff_file_content;
use crate::options::{DiffOptions, DisplayOptions, FileArgument};
use crate::parse::guess_language::language_name;
use crate::parse::syntax::{AtomKind, MatchKind, MatchedPos, StringKind, TokenKind};
use crate::summary::FileFormat;

/// Limits that decide when difftastic gives up on a structural diff
/// and falls back to a line-based one.
#[derive(Debug, Clone)]
pub struct Options {
    /// Maximum number of graph vertices to explore before falling back.
    pub graph_limit: usize,
    /// Files larger than this many bytes are diffed line by line.
    pub byte_limit: usize,
    /// Maximum number of tree-sitter parse errors before falling back.
    pub parse_error_limit: usize,
    /// Don't report changes inside comments.
    pub ignore_comments: bool,
}

impl Default for Options {
    fn default() -> Self {
        let defaults = DiffOptions::default();
        Self {
            graph_limit: defaults.graph_limit,
            byte_limit: defaults.byte_limit,
            parse_error_limit: defaults.parse_error_limit,
            ignore_comments: defaults.ignore_comments,
        }
    }
}

/// How the two files were compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    /// The files were parsed and their syntax trees diffed.
    Structural { language: &'static str },
    /// No parser exists for this file, so it was diffed line by line.
    PlainText,
    /// A parser exists, but a limit was exceeded, so it was diffed
    /// line by line.
    Fallback { reason: String },
}

/// Why a span was reported as changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// A token with no counterpart on the other side.
    Novel,
    /// A word that changed inside a string or comment that was
    /// otherwise matched.
    NovelWord,
    /// An unchanged word inside a string or comment that changed
    /// elsewhere. Usually rendered less prominently than `NovelWord`.
    UnchangedPartOfNovelItem,
}

/// The syntactic category difftastic assigned to a span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Highlight {
    Normal,
    Delimiter,
    String,
    Type,
    Keyword,
    Comment,
    TreeSitterError,
}

/// A changed span on a single line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Zero-based line number.
    pub line: u32,
    /// Start byte offset within the line.
    pub start: u32,
    /// End byte offset within the line (exclusive).
    pub end: u32,
    pub kind: ChangeKind,
    pub highlight: Highlight,
}

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub method: Method,
    /// Changed spans in the old file, ordered by position.
    pub lhs_changes: Vec<Change>,
    /// Changed spans in the new file, ordered by position.
    pub rhs_changes: Vec<Change>,
    /// Every line of both files paired up as difftastic would display
    /// them side by side. `None` means the line has no counterpart.
    /// Line numbers are zero-based.
    pub aligned_lines: Vec<(Option<u32>, Option<u32>)>,
}

impl FileDiff {
    /// Whether difftastic found any change. Formatting-only changes
    /// (whitespace, reflowed lines) are not changes in a structural diff.
    pub fn has_changes(&self) -> bool {
        !self.lhs_changes.is_empty() || !self.rhs_changes.is_empty()
    }
}

/// Diff two versions of a file. `path` is only used to guess the
/// language, so it doesn't have to exist on disk.
pub fn diff(path: &str, lhs_src: &str, rhs_src: &str, options: &Options) -> FileDiff {
    let file_arg = FileArgument::from_path_argument(OsStr::new(path));
    let diff_options = DiffOptions {
        graph_limit: options.graph_limit,
        byte_limit: options.byte_limit,
        parse_error_limit: options.parse_error_limit,
        ignore_comments: options.ignore_comments,
        ..DiffOptions::default()
    };
    let result = diff_file_content(
        path,
        None,
        &file_arg,
        &file_arg,
        lhs_src,
        rhs_src,
        &DisplayOptions::default(),
        &diff_options,
        &[],
    );

    let method = match result.file_format {
        FileFormat::SupportedLanguage(language) => Method::Structural {
            language: language_name(language),
        },
        FileFormat::TextFallback { reason } => Method::Fallback { reason },
        // We only receive text, so Binary can't occur.
        FileFormat::PlainText | FileFormat::Binary => Method::PlainText,
    };

    let lhs_lines: Vec<&str> = lhs_src.split('\n').collect();
    let rhs_lines: Vec<&str> = rhs_src.split('\n').collect();
    let aligned_lines = all_matched_lines_filled(
        &result.lhs_positions,
        &result.rhs_positions,
        &lhs_lines,
        &rhs_lines,
    )
    .into_iter()
    .map(|(lhs, rhs)| (lhs.map(|l| l.0), rhs.map(|l| l.0)))
    .collect();

    FileDiff {
        method,
        lhs_changes: changes(&result.lhs_positions),
        rhs_changes: changes(&result.rhs_positions),
        aligned_lines,
    }
}

fn changes(positions: &[MatchedPos]) -> Vec<Change> {
    let mut changes: Vec<Change> = positions
        .iter()
        .filter_map(|mp| {
            let (kind, highlight) = match &mp.kind {
                MatchKind::Novel { highlight } => (ChangeKind::Novel, highlight),
                MatchKind::NovelWord { highlight } => (ChangeKind::NovelWord, highlight),
                MatchKind::UnchangedPartOfNovelItem { highlight, .. } => {
                    (ChangeKind::UnchangedPartOfNovelItem, highlight)
                }
                MatchKind::UnchangedToken { .. } | MatchKind::Ignored { .. } => return None,
            };
            Some(Change {
                line: mp.pos.line.0,
                start: mp.pos.start_col,
                end: mp.pos.end_col,
                kind,
                highlight: highlight_from_token(highlight),
            })
        })
        .collect();
    changes.sort_by_key(|c| (c.line, c.start));
    changes
}

fn highlight_from_token(token: &TokenKind) -> Highlight {
    match token {
        TokenKind::Delimiter => Highlight::Delimiter,
        TokenKind::Atom(atom) => match atom {
            AtomKind::String(StringKind::StringLiteral) => Highlight::String,
            AtomKind::String(StringKind::Text) => Highlight::Normal,
            AtomKind::Keyword => Highlight::Keyword,
            AtomKind::Comment => Highlight::Comment,
            AtomKind::Type => Highlight::Type,
            AtomKind::Normal | AtomKind::CanIgnore => Highlight::Normal,
            AtomKind::TreeSitterError => Highlight::TreeSitterError,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reformatting_is_not_a_change() {
        let lhs = "fn f() { g(a, b); }\n";
        let rhs = "fn f() {\n    g(\n        a,\n        b,\n    );\n}\n";
        let res = diff("f.rs", lhs, rhs, &Options::default());

        assert_eq!(res.method, Method::Structural { language: "Rust" });
        // Trailing commas are ignorable punctuation, so nothing changed.
        assert!(!res.has_changes());
        assert!(res.aligned_lines.len() >= rhs.lines().count());
    }

    #[test]
    fn test_novel_word_in_string() {
        let lhs = "fn f() { format!(\"{a}{b}\"); }\n";
        let rhs = "fn f() { format!(\"{a}{c}{b}\"); }\n";
        let res = diff("f.rs", lhs, rhs, &Options::default());

        let novel_words: String = res
            .rhs_changes
            .iter()
            .filter(|c| c.kind == ChangeKind::NovelWord)
            .map(|c| &rhs[c.start as usize..c.end as usize])
            .collect();
        // Word diffs inside strings aren't slider-corrected, so the
        // inserted `{c}` is reported as `c}{`.
        assert_eq!(novel_words, "c}{");
        assert!(res
            .rhs_changes
            .iter()
            .any(|c| c.kind == ChangeKind::UnchangedPartOfNovelItem));
    }

    #[test]
    fn test_unknown_language_is_plain_text() {
        let res = diff("notes.unknownext", "a\n", "b\n", &Options::default());
        assert_eq!(res.method, Method::PlainText);
        assert!(res.has_changes());
    }
}
