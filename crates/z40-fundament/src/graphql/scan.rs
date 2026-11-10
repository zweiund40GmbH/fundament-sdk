//! The bracket scan of a raw query text (`docs/protocol.md` §4.2), run **before** the query is parsed.
//!
//! The GraphQL parser builds nested values (`[[[…]]]`, `{a: {a: …}}`) and selection sets recursively; it
//! limits the recursion of selection sets only. A few kilobytes of nested brackets in an argument or a
//! variable default overflow the stack and abort the whole process – on any async-graphql endpoint, not
//! only the subgraph. This scan walks the text once, without recursion, and rejects queries in which too
//! many brackets are open at once, so the parser's recursion stays shallow.

use super::{SubgraphLimits, Violation};

/// Checks that at most [`SubgraphLimits::max_bracket_depth`] brackets `[`, `{`, `(` are open at once in
/// `query`, outside strings, block strings and comments – else [`Violation::BracketDepth`]. Linear in the
/// length of `query`, no allocation.
///
/// Call it before handing a query to async-graphql on **every** endpoint, not only the subgraph (the
/// [`super::SubgraphEndpoint`] does). Selection sets count too: keep the bracket depth at least the
/// selection nesting plus the depth of the argument values a client needs. Unbalanced brackets are not
/// rejected here – the parser reports them – so no valid document is refused for its shape.
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
            b'"' if bytes.get(i + 1..i + 3) == Some(b"\"\"") => skip_block_string(bytes, i + 3),
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
/// parser rejects it – at a line terminator, like the lexer.
fn skip_string(bytes: &[u8], mut i: usize) -> usize {
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\\' => i += 2,
            b'"' => return i + 1,
            b'\n' | b'\r' => return i,
            _ => i += 1,
        }
    }
    i
}

/// The index after a block string whose content starts at `i`: it ends at `"""`; `\"""` is an escaped
/// quote triple, every other backslash is literal.
fn skip_block_string(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() {
        let rest = bytes.get(i..).unwrap_or_default();
        if rest.starts_with(b"\\\"\"\"") {
            i += 4;
        } else if rest.starts_with(b"\"\"\"") {
            return i + 3;
        } else {
            i += 1;
        }
    }
    i
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
