//! The bracket scan of a raw query text (`docs/protocol.md` §4.2), run **before** the query is parsed.
//!
//! The GraphQL parser builds nested values (`[[[…]]]`, `{a: {a: …}}`) and selection sets recursively; it
//! limits the recursion of selection sets only. A few kilobytes of nested brackets in an argument or a
//! variable default overflow the stack and abort the whole process – on any async-graphql endpoint, not
//! only the subgraph. This scan walks the text once, without recursion, and rejects queries in which too
//! many brackets are open at once, so the parser's recursion stays shallow.
//!
//! The scan must never skip text the parser reads as tokens. Strings and comments are skipped exactly as
//! the grammar of async-graphql 7.2 reads them, with one exception the grammar backtracks on: a block
//! string without a closing `"""` is read by the parser as `""` followed by an ordinary string, and the
//! text after it is parsed. The scan therefore rejects it ([`Violation::UnterminatedString`]) instead of
//! skipping to the end. Every other string the parser cannot read makes the whole parse fail at that
//! point (nothing else in an executable document starts with `"`), so text skipped behind it is never
//! parsed.

use super::{SubgraphLimits, Violation};

/// Checks that at most [`SubgraphLimits::max_bracket_depth`] brackets `[`, `{`, `(` are open at once in
/// `query`, outside strings, block strings and comments – else [`Violation::BracketDepth`] – and that
/// every block string is closed – else [`Violation::UnterminatedString`]. Linear in the length of `query`,
/// no allocation.
///
/// Call it before handing a query to async-graphql on **every** endpoint, not only the subgraph (the
/// [`super::SubgraphEndpoint`] does). Selection sets count too: keep the bracket depth at least the
/// selection nesting plus the depth of the argument values a client needs. Unbalanced brackets are not
/// rejected here – the parser reports them. The only valid documents refused are those reading `"""`
/// as an empty string followed by another string (`["""x"]`).
pub fn scan_query(query: &str, limits: &SubgraphLimits) -> Result<(), Violation> {
    let max = limits.max_bracket_depth();
    let bytes = query.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    while let Some(&b) = bytes.get(i) {
        i = match b {
            b'[' | b'{' | b'(' => {
                depth += 1;
                if depth > max {
                    return Err(Violation::BracketDepth);
                }
                i + 1
            }
            b']' | b'}' | b')' => {
                depth = depth.saturating_sub(1);
                i + 1
            }
            b'#' => skip_comment(bytes, i + 1),
            b'"' if bytes.get(i + 1..i + 3) == Some(b"\"\"") => {
                skip_block_string(bytes, i + 3).ok_or(Violation::UnterminatedString)?
            }
            b'"' => skip_string(bytes, i + 1),
            _ => i + 1,
        };
    }
    Ok(())
}

/// The index after a comment whose text starts at `i`: comments end at a line terminator.
fn skip_comment(bytes: &[u8], mut i: usize) -> usize {
    while let Some(&b) = bytes.get(i) {
        if b == b'\n' || b == b'\r' {
            break;
        }
        i += 1;
    }
    i
}

/// The index after a string whose content starts at `i`: it ends at an unescaped `"`, or – invalid, the
/// parser rejects it – at a line terminator, also one right after a backslash.
fn skip_string(bytes: &[u8], mut i: usize) -> usize {
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\\' if matches!(bytes.get(i + 1), Some(b'\n' | b'\r')) => return i + 1,
            b'\\' => i += 2,
            b'"' => return i + 1,
            b'\n' | b'\r' => return i,
            _ => i += 1,
        }
    }
    i
}

/// The index after a block string whose content starts at `i`: it ends at `"""`; `\"""` is an escaped
/// quote triple, every other backslash is literal. `None` when the input ends before the closing `"""`.
fn skip_block_string(bytes: &[u8], mut i: usize) -> Option<usize> {
    while i < bytes.len() {
        let rest = bytes.get(i..).unwrap_or_default();
        if rest.starts_with(b"\\\"\"\"") {
            i += 4;
        } else if rest.starts_with(b"\"\"\"") {
            return Some(i + 3);
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "scan_property_tests.rs"]
mod property_tests;
