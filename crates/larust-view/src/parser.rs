use crate::ast::Node;
use crate::error::ParseError;

const KEYWORDS: &[&str] = &[
    "extends",
    "section",
    "endsection",
    "yield",
    "if",
    "elseif",
    "else",
    "endif",
    "foreach",
    "endforeach",
    "push",
    "endpush",
    "stack",
    "csrf",
    "global",
    "globals",
    "endglobals",
    "live",
    "larustscripts",
    "loadonce",
    "endloadonce",
];

/// Directives that end whatever block is currently being parsed (an
/// `@if`/`@foreach`/`@section` body). Matched against regardless of what
/// the caller "expects" — the caller checks the returned tag itself and
/// produces a clear "expected @endif, found @endforeach" error on mismatch,
/// rather than the parser silently overrunning into the wrong block.
const CLOSERS: &[&str] = &[
    "else",
    "elseif",
    "endif",
    "endsection",
    "endforeach",
    "endpush",
    "endglobals",
    "endloadonce",
];

/// A directive that ended the block currently being parsed. `elseif_cond`
/// is only ever set when `tag == "elseif"` — its condition is consumed
/// right where the closer itself is recognized (see `read_closer`), since
/// `@elseif`'s condition logically belongs to whatever nested `@if` it
/// desugars into (see `parse_if_tail`), not to this closer signal.
/// `tag` borrows from `KEYWORDS` (always `'static`), not the source text —
/// no allocation for something as cheap and frequent as a block boundary.
struct Closer {
    tag: &'static str,
    elseif_cond: Option<String>,
}

pub fn parse(source: &str) -> Result<Vec<Node>, ParseError> {
    let mut cursor = Cursor::new(source);
    let (nodes, closer) = parse_nodes(&mut cursor)?;
    if let Some(Closer { tag, .. }) = closer {
        return Err(ParseError::new(format!(
            "unexpected @{tag} with no matching opening directive"
        )));
    }
    Ok(nodes)
}

struct Cursor<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn advance(&mut self, n: usize) {
        self.pos += n;
    }
}

enum MarkerKind {
    Escaped,
    Raw,
    At(&'static str),
}

fn next_marker(s: &str) -> Option<(usize, MarkerKind)> {
    let at = find_next_at_directive(s).map(|(p, kw)| (p, MarkerKind::At(kw)));
    let raw = s.find("{!!").map(|p| (p, MarkerKind::Raw));
    let esc = s.find("{{").map(|p| (p, MarkerKind::Escaped));

    [at, raw, esc].into_iter().flatten().min_by_key(|(p, _)| *p)
}

/// Only treats `@` as a directive marker when immediately followed by a
/// recognized keyword at a word boundary — otherwise a literal `@` in HTML
/// content (an email address, an `@media` CSS-like string, etc.) would
/// wrongly be parsed as a directive.
fn find_next_at_directive(s: &str) -> Option<(usize, &'static str)> {
    let mut search_from = 0;
    while let Some(rel) = s[search_from..].find('@') {
        let pos = search_from + rel;
        let after = &s[pos + 1..];
        let hit = KEYWORDS.iter().find(|kw| {
            after.starts_with(**kw)
                && after[kw.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        });
        if let Some(kw) = hit {
            return Some((pos, kw));
        }
        search_from = pos + 1;
    }
    None
}

/// Builds the `Closer` signal for a just-consumed closing directive.
/// `@elseif`'s condition is consumed here, not by whatever handles the
/// closer — it belongs to the nested `@if` it desugars into (see
/// `parse_if_tail`), which doesn't exist yet at this point in parsing.
fn read_closer(cur: &mut Cursor, kw: &'static str) -> Result<Closer, ParseError> {
    let elseif_cond = if kw == "elseif" {
        Some(parse_paren_expr(cur)?)
    } else {
        None
    };
    Ok(Closer {
        tag: kw,
        elseif_cond,
    })
}

fn parse_nodes(cur: &mut Cursor) -> Result<(Vec<Node>, Option<Closer>), ParseError> {
    let mut nodes = Vec::new();

    loop {
        match next_marker(cur.rest()) {
            None => {
                let text = cur.rest();
                if !text.is_empty() {
                    nodes.push(Node::Text(text.to_string()));
                }
                cur.advance(text.len());
                return Ok((nodes, None));
            }
            Some((offset, kind)) => {
                if offset > 0 {
                    nodes.push(Node::Text(cur.rest()[..offset].to_string()));
                }
                cur.advance(offset);

                match kind {
                    MarkerKind::Escaped => {
                        let expr = parse_braces(cur, "{{", "}}")?;
                        nodes.push(Node::Interpolate { expr, escape: true });
                    }
                    MarkerKind::Raw => {
                        let expr = parse_braces(cur, "{!!", "!!}")?;
                        nodes.push(Node::Interpolate {
                            expr,
                            escape: false,
                        });
                    }
                    MarkerKind::At(kw) => {
                        cur.advance(1 + kw.len()); // consume "@" + keyword

                        if CLOSERS.contains(&kw) {
                            return Ok((nodes, Some(read_closer(cur, kw)?)));
                        }

                        match kw {
                            "extends" => {
                                let name = parse_quoted_arg(cur)?;
                                nodes.push(Node::Extends(name));
                            }
                            "yield" => {
                                let name = parse_quoted_arg(cur)?;
                                nodes.push(Node::Yield(name));
                            }
                            "section" => {
                                let name = parse_quoted_arg(cur)?;
                                let (body, closer) = parse_nodes(cur)?;
                                expect_closer(closer, "endsection")?;
                                nodes.push(Node::Section { name, body });
                            }
                            "if" => {
                                let cond = parse_paren_expr(cur)?;
                                let (then_branch, else_branch) = parse_if_tail(cur)?;
                                nodes.push(Node::If {
                                    cond,
                                    then_branch,
                                    else_branch,
                                });
                            }
                            "foreach" => {
                                let raw = parse_paren_expr(cur)?;
                                let (binding, iter) = split_foreach(&raw)?;
                                let (body, closer) = parse_nodes(cur)?;
                                expect_closer(closer, "endforeach")?;
                                nodes.push(Node::Foreach {
                                    binding,
                                    iter,
                                    body,
                                });
                            }
                            "push" => {
                                let name = parse_quoted_arg(cur)?;
                                let (body, closer) = parse_nodes(cur)?;
                                expect_closer(closer, "endpush")?;
                                nodes.push(Node::Push { name, body });
                            }
                            "stack" => {
                                let name = parse_quoted_arg(cur)?;
                                nodes.push(Node::Stack(name));
                            }
                            "csrf" => {
                                nodes.push(Node::Csrf);
                            }
                            "global" => {
                                let (name, fallback) = parse_global_args(cur)?;
                                nodes.push(Node::Global { name, fallback });
                            }
                            "globals" => {
                                let entries = parse_globals_block(cur)?;
                                nodes.push(Node::Globals(entries));
                            }
                            "live" => {
                                let (name, props) = parse_live_args(cur)?;
                                nodes.push(Node::Live { name, props });
                            }
                            "larustscripts" => {
                                nodes.push(Node::LarustScripts);
                            }
                            "loadonce" => {
                                let (body, closer) = parse_nodes(cur)?;
                                expect_closer(closer, "endloadonce")?;
                                nodes.push(Node::LoadOnce(body));
                            }
                            other => {
                                return Err(ParseError::new(format!("unknown directive @{other}")))
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Parses everything after an `@if(...)`/`@elseif(...)` condition, through
/// whichever of `@elseif`/`@else`/`@endif` ends it — recursively
/// desugaring any `@elseif` chain into nested `Node::If`s in the else
/// branch. `@if(a) X @elseif(b) Y @else Z @endif` builds exactly the same
/// tree hand-nesting `@if(a) X @else @if(b) Y @else Z @endif @endif`
/// would, so `larust-macros`' codegen needs no changes at all to support
/// `@elseif` — it only ever sees plain, possibly-nested `Node::If`s.
fn parse_if_tail(cur: &mut Cursor) -> Result<(Vec<Node>, Vec<Node>), ParseError> {
    let (then_branch, closer) = parse_nodes(cur)?;
    match closer {
        Some(Closer { tag: "endif", .. }) => Ok((then_branch, Vec::new())),
        Some(Closer { tag: "else", .. }) => {
            let (else_branch, closer2) = parse_nodes(cur)?;
            expect_closer(closer2, "endif")?;
            Ok((then_branch, else_branch))
        }
        Some(Closer {
            tag: "elseif",
            elseif_cond: Some(cond),
        }) => {
            let (nested_then, nested_else) = parse_if_tail(cur)?;
            let else_branch = vec![Node::If {
                cond,
                then_branch: nested_then,
                else_branch: nested_else,
            }];
            Ok((then_branch, else_branch))
        }
        other => Err(unexpected_closer(other, "@elseif, @else, or @endif")),
    }
}

fn parse_braces(cur: &mut Cursor, open: &str, close: &str) -> Result<String, ParseError> {
    cur.advance(open.len());
    let s = cur.rest();
    let end = s
        .find(close)
        .ok_or_else(|| ParseError::new(format!("unterminated {open} ... {close}")))?;
    let expr = s[..end].trim().to_string();
    cur.advance(end + close.len());
    Ok(expr)
}

fn parse_quoted_arg(cur: &mut Cursor) -> Result<String, ParseError> {
    skip_ws(cur);
    expect_char(cur, '(')?;
    skip_ws(cur);
    let value = parse_quoted_string(cur)?;
    skip_ws(cur);
    expect_char(cur, ')')?;
    Ok(value)
}

/// Scans a `'...'`- or `"..."`-quoted string starting at the current
/// position (the opening quote itself not yet consumed) — shared by
/// `parse_quoted_arg` (`@extends('...')`, `@yield('...')`, ...) and
/// `parse_live_args` (`@live('name', ...)`'s own name argument, which needs
/// just the quoted-string scan without `parse_quoted_arg`'s surrounding
/// `(`/`)` handling, since more may follow after the name).
fn parse_quoted_string(cur: &mut Cursor) -> Result<String, ParseError> {
    let quote = expect_one_of(cur, &['\'', '"'])?;

    let s = cur.rest();
    let end = s
        .find(quote)
        .ok_or_else(|| ParseError::new("unterminated string in directive argument"))?;
    let value = s[..end].to_string();
    cur.advance(end + 1); // consume through the closing quote
    Ok(value)
}

/// Parses `@global(name)` or `@global(name, fallback)`. `name` is a bare
/// identifier, not a quoted string — deliberately different from
/// `parse_quoted_arg`: it matches the bare-identifier style the `@globals`
/// block itself uses (`title = "..."`), so the same literal token names the
/// placeholder and its setter with no quote-mark mismatch between the two.
/// `fallback`, when present, is any Rust expression up to the matching
/// closing paren (via `scan_to_matching_close_paren`, shared with
/// `parse_paren_expr`) — same free-form-expression convention as a
/// `@globals` assignment's own right-hand side.
fn parse_global_args(cur: &mut Cursor) -> Result<(String, Option<String>), ParseError> {
    skip_ws(cur);
    expect_char(cur, '(')?;
    skip_ws(cur);

    let s = cur.rest();
    let end = s
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    if end == 0 {
        return Err(ParseError::new("expected a variable name in @global(...)"));
    }
    let name = s[..end].to_string();
    cur.advance(end);
    skip_ws(cur);

    match cur.rest().chars().next() {
        Some(')') => {
            cur.advance(1);
            Ok((name, None))
        }
        Some(',') => {
            cur.advance(1);
            skip_ws(cur);
            let fallback = scan_to_matching_close_paren(cur)?;
            if fallback.is_empty() {
                return Err(ParseError::new(
                    "expected a fallback expression after ',' in @global(...)",
                ));
            }
            Ok((name, Some(fallback)))
        }
        Some(c) => Err(ParseError::new(format!(
            "expected ',' or ')' in @global(...), found '{c}'"
        ))),
        None => Err(ParseError::new(
            "expected ',' or ')' in @global(...), found end of template",
        )),
    }
}

/// Parses an `@globals ... @endglobals` block body: one `name = expr`
/// assignment per non-blank line. Deliberately **not** routed through
/// `parse_nodes` — this is a different grammar (assignment lines), not
/// blade markup — so it scans for the literal `"@endglobals"` marker the
/// same way `parse_quoted_arg` scans for a closing quote, rather than
/// tokenizing through the normal directive dispatch.
fn parse_globals_block(cur: &mut Cursor) -> Result<Vec<(String, String)>, ParseError> {
    let s = cur.rest();
    let end = s
        .find("@endglobals")
        .ok_or_else(|| ParseError::new("unterminated @globals block, expected @endglobals"))?;
    let block_text = &s[..end];
    let entries = parse_globals_entries(block_text)?;
    cur.advance(end + "@endglobals".len());
    Ok(entries)
}

fn parse_globals_entries(block_text: &str) -> Result<Vec<(String, String)>, ParseError> {
    let mut entries = Vec::new();
    for line in block_text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let split = find_assignment_split(line).ok_or_else(|| {
            ParseError::new(format!(
                "expected `name = expression` in @globals block, found: `{line}`"
            ))
        })?;
        let name = line[..split].trim().to_string();
        let expr = line[split + 1..].trim().to_string();
        // Must match the exact character class `parse_global_args` accepts
        // for `@global(name)`'s own placeholder name — otherwise a typo'd
        // or malformed name here (a stray space, a hyphen, ...) would
        // silently register under a key no `@global(...)` placeholder
        // could ever match, producing a dead map entry with no error at
        // all: the placeholder would just quietly fall through to its
        // `fallback` (or empty) instead.
        if !is_valid_global_name(&name) {
            return Err(ParseError::new(format!(
                "invalid variable name `{name}` in @globals block — only letters, digits, and \
                 underscores are allowed, matching @global(name)'s own placeholder syntax"
            )));
        }
        if expr.is_empty() {
            return Err(ParseError::new(format!(
                "expected an expression after '=' in @globals block, found: `{line}`"
            )));
        }
        entries.push((name, expr));
    }
    Ok(entries)
}

/// Parses `@live('name')` or `@live('name', { prop: expr, ... })`. The
/// component name reuses `parse_quoted_string`, the same convention
/// `@extends('...')` uses; the optional props object is a brace-delimited,
/// comma-separated `key: expr` list — see `parse_prop_entries`.
fn parse_live_args(cur: &mut Cursor) -> Result<(String, Vec<(String, String)>), ParseError> {
    skip_ws(cur);
    expect_char(cur, '(')?;
    skip_ws(cur);
    let name = parse_quoted_string(cur)?;
    skip_ws(cur);

    match cur.rest().chars().next() {
        Some(')') => {
            cur.advance(1);
            Ok((name, Vec::new()))
        }
        Some(',') => {
            cur.advance(1);
            skip_ws(cur);
            expect_char(cur, '{')?;
            let props = parse_prop_entries(cur)?;
            skip_ws(cur);
            expect_char(cur, ')')?;
            Ok((name, props))
        }
        Some(c) => Err(ParseError::new(format!(
            "expected ',' or ')' in @live(...), found '{c}'"
        ))),
        None => Err(ParseError::new(
            "expected ',' or ')' in @live(...), found end of template",
        )),
    }
}

/// Parses the interior of a `@live(..., { key: expr, key2: expr2 })` props
/// object — cursor positioned just past the opening `{`. Scans to the
/// matching `}`, tracking one *combined* nesting depth over `(`/`{`/`[`
/// (any of which may legitimately appear inside a prop's own expression,
/// e.g. `{ items: vec![1, 2] }`) rather than three independent counters —
/// sufficient here because this scan only needs to know when it's back at
/// the outer object's own closing brace, not whether the source's brackets
/// are individually well-matched (a genuine mismatch surfaces later as a
/// `syn::parse_str` error in `larust-macros` instead). Quoted spans are
/// skipped with the same backslash-escape handling as
/// `scan_to_matching_close_paren`. Advances the cursor past the matching
/// closing `}`.
fn parse_prop_entries(cur: &mut Cursor) -> Result<Vec<(String, String)>, ParseError> {
    let s = cur.rest();
    let mut depth: i32 = 1;
    let mut end = None;
    let mut chars = s.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        match c {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            '"' | '\'' => {
                let quote = c;
                while let Some((_, ch)) = chars.next() {
                    if ch == '\\' {
                        chars.next();
                        continue;
                    }
                    if ch == quote {
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    let end = end.ok_or_else(|| ParseError::new("unterminated '{' in @live(...) props"))?;
    let body = &s[..end];
    cur.advance(end + 1);

    split_prop_entries(body)
}

/// Splits a `@live(...)` props body on top-level commas (honoring nested
/// brackets and quoted spans, same depth-tracking technique as
/// `parse_prop_entries`) and each resulting entry on its first colon (via
/// `find_prop_colon`). A trailing comma, or an entirely empty `{}`, leaves
/// a blank final segment — tolerated, not an error, matching
/// `parse_globals_entries`'s own "blank lines are skipped" convention.
fn split_prop_entries(body: &str) -> Result<Vec<(String, String)>, ParseError> {
    let mut entries = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0;
    let bytes = body.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'(' | b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b')' | b'}' | b']' => {
                depth -= 1;
                i += 1;
            }
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b',' if depth == 0 => {
                push_prop_entry(&mut entries, body[start..i].trim())?;
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    push_prop_entry(&mut entries, body[start..].trim())?;

    Ok(entries)
}

fn push_prop_entry(entries: &mut Vec<(String, String)>, entry: &str) -> Result<(), ParseError> {
    if entry.is_empty() {
        return Ok(());
    }
    let split = find_prop_colon(entry).ok_or_else(|| {
        ParseError::new(format!(
            "expected `name: expression` in @live(...) props, found: `{entry}`"
        ))
    })?;
    let name = entry[..split].trim().to_string();
    let expr = entry[split + 1..].trim().to_string();
    // Same character class `@global`/`@globals` already require of a
    // placeholder name — kept consistent rather than inventing a separate
    // rule for prop names.
    if !is_valid_global_name(&name) {
        return Err(ParseError::new(format!(
            "invalid prop name `{name}` in @live(...) — only letters, digits, and underscores \
             are allowed"
        )));
    }
    if expr.is_empty() {
        return Err(ParseError::new(format!(
            "expected an expression after ':' in @live(...) props, found: `{entry}`"
        )));
    }
    entries.push((name, expr));
    Ok(())
}

/// Finds the byte offset of the first unquoted `:` in a prop entry. Unlike
/// `find_assignment_split`, no operator-adjacency check (for `!=`/`<=`/...)
/// or bracket-depth tracking is needed: a prop key is required to be a bare
/// identifier immediately before this colon, and identifiers can't contain
/// `:` or brackets, so the first unquoted `:` encountered while scanning
/// left-to-right is always the key/value separator — never a `::` path
/// separator or a nested struct-literal field's own colon, both of which
/// can only occur *after* it, inside `expr`.
fn find_prop_colon(entry: &str) -> Option<usize> {
    let bytes = entry.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b':' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// The same character class `parse_global_args` accepts for `@global(name)`
/// — alphanumeric plus underscore, non-empty. Shared so a `@globals` block's
/// assignment names can never silently drift out of sync with what a
/// `@global(...)` placeholder is able to match.
fn is_valid_global_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Finds the byte offset of the assignment `=` in a `name = expr` line —
/// not just the first `=`, since `expr` may itself contain comparison
/// operators (`==`, `!=`, `<=`, `>=`) or a string literal containing `=`.
/// Skips over quoted spans (with the same backslash-escape handling as
/// `parse_paren_expr`) and rejects an `=` that's adjacent to `!`/`<`/`>`/`=`
/// (part of a multi-char operator), returning the first truly isolated `=`.
fn find_assignment_split(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'=' => {
                let prev = if i == 0 { None } else { Some(bytes[i - 1]) };
                let next = bytes.get(i + 1).copied();
                let is_operator_char =
                    |b: Option<u8>| matches!(b, Some(b'!') | Some(b'<') | Some(b'>') | Some(b'='));
                if !is_operator_char(prev) && next != Some(b'=') {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// Extracts the balanced, string-literal-aware contents of `@if(...)` /
/// `@foreach(...)` — an expression may itself contain parens
/// (`user.age > (18 + 1)`) or a string literal containing a paren
/// (`name == "(test)"`), both of which must not prematurely close the
/// directive's own outer parens.
fn parse_paren_expr(cur: &mut Cursor) -> Result<String, ParseError> {
    skip_ws(cur);
    expect_char(cur, '(')?;
    scan_to_matching_close_paren(cur)
}

/// Scans from the current position — already *past* an opening `(` — to its
/// matching `)`, honoring nested parens and string literals containing
/// parens (`user.age > (18 + 1)`, `name == "(test)"`), and advances the
/// cursor past that closing paren. Shared by `parse_paren_expr` (for
/// `@if(...)`/`@foreach(...)`) and `parse_global_args` (for `@global(name,
/// fallback)`'s optional second argument).
fn scan_to_matching_close_paren(cur: &mut Cursor) -> Result<String, ParseError> {
    let s = cur.rest();
    let mut depth: i32 = 1;
    let mut end = None;
    let mut chars = s.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            '"' | '\'' => {
                let quote = c;
                while let Some((_, ch)) = chars.next() {
                    if ch == '\\' {
                        chars.next();
                        continue;
                    }
                    if ch == quote {
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    let end = end.ok_or_else(|| ParseError::new("unterminated '(' in directive"))?;
    let expr = s[..end].trim().to_string();
    cur.advance(end + 1);
    Ok(expr)
}

fn split_foreach(raw: &str) -> Result<(String, String), ParseError> {
    let idx = raw
        .find(" in ")
        .ok_or_else(|| ParseError::new("expected `binding in iterable` in @foreach(...)"))?;
    let binding = raw[..idx].trim().to_string();
    let iter = raw[idx + 4..].trim().to_string();
    if binding.is_empty() || iter.is_empty() {
        return Err(ParseError::new(
            "expected `binding in iterable` in @foreach(...)",
        ));
    }
    Ok((binding, iter))
}

fn expect_closer(found: Option<Closer>, expected: &str) -> Result<(), ParseError> {
    match found {
        Some(Closer { tag, .. }) if tag == expected => Ok(()),
        other => Err(unexpected_closer(other, &format!("@{expected}"))),
    }
}

fn unexpected_closer(found: Option<Closer>, expected: &str) -> ParseError {
    match found {
        Some(Closer { tag, .. }) => ParseError::new(format!("expected {expected}, found @{tag}")),
        None => ParseError::new(format!("unexpected end of template, expected {expected}")),
    }
}

fn skip_ws(cur: &mut Cursor) {
    let n = cur.rest().len() - cur.rest().trim_start().len();
    cur.advance(n);
}

fn expect_char(cur: &mut Cursor, expected: char) -> Result<(), ParseError> {
    match cur.rest().chars().next() {
        Some(c) if c == expected => {
            cur.advance(c.len_utf8());
            Ok(())
        }
        Some(c) => Err(ParseError::new(format!(
            "expected '{expected}', found '{c}'"
        ))),
        None => Err(ParseError::new(format!(
            "expected '{expected}', found end of template"
        ))),
    }
}

fn expect_one_of(cur: &mut Cursor, options: &[char]) -> Result<char, ParseError> {
    match cur.rest().chars().next() {
        Some(c) if options.contains(&c) => {
            cur.advance(c.len_utf8());
            Ok(c)
        }
        Some(c) => Err(ParseError::new(format!(
            "expected one of {options:?}, found '{c}'"
        ))),
        None => Err(ParseError::new(format!(
            "expected one of {options:?}, found end of template"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_text() {
        let nodes = parse("hello world").unwrap();
        assert_eq!(nodes, vec![Node::Text("hello world".to_string())]);
    }

    #[test]
    fn parses_escaped_interpolation() {
        let nodes = parse("Hi {{ user.name }}!").unwrap();
        assert_eq!(
            nodes,
            vec![
                Node::Text("Hi ".to_string()),
                Node::Interpolate {
                    expr: "user.name".to_string(),
                    escape: true
                },
                Node::Text("!".to_string()),
            ]
        );
    }

    #[test]
    fn parses_raw_interpolation() {
        let nodes = parse("{!! html !!}").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Interpolate {
                expr: "html".to_string(),
                escape: false
            }]
        );
    }

    #[test]
    fn literal_at_sign_is_not_a_directive() {
        let nodes = parse("contact: hello@example.com").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Text("contact: hello@example.com".to_string())]
        );
    }

    #[test]
    fn parses_if_else() {
        let nodes = parse("@if(user.admin) admin @else guest @endif").unwrap();
        assert_eq!(
            nodes,
            vec![Node::If {
                cond: "user.admin".to_string(),
                then_branch: vec![Node::Text(" admin ".to_string())],
                else_branch: vec![Node::Text(" guest ".to_string())],
            }]
        );
    }

    #[test]
    fn parses_if_without_else() {
        let nodes = parse("@if(ok) yes @endif").unwrap();
        assert_eq!(
            nodes,
            vec![Node::If {
                cond: "ok".to_string(),
                then_branch: vec![Node::Text(" yes ".to_string())],
                else_branch: vec![],
            }]
        );
    }

    #[test]
    fn parses_elseif_without_a_trailing_else() {
        // Desugars into a nested `Node::If` in the else branch — exactly
        // what hand-nesting `@if(a) X @else @if(b) Y @endif @endif` would
        // produce, proving `@elseif` needs no codegen changes.
        let nodes = parse("@if(a) X @elseif(b) Y @endif").unwrap();
        assert_eq!(
            nodes,
            vec![Node::If {
                cond: "a".to_string(),
                then_branch: vec![Node::Text(" X ".to_string())],
                else_branch: vec![Node::If {
                    cond: "b".to_string(),
                    then_branch: vec![Node::Text(" Y ".to_string())],
                    else_branch: vec![],
                }],
            }]
        );
    }

    #[test]
    fn parses_elseif_with_a_trailing_else() {
        let nodes = parse("@if(a) X @elseif(b) Y @else Z @endif").unwrap();
        assert_eq!(
            nodes,
            vec![Node::If {
                cond: "a".to_string(),
                then_branch: vec![Node::Text(" X ".to_string())],
                else_branch: vec![Node::If {
                    cond: "b".to_string(),
                    then_branch: vec![Node::Text(" Y ".to_string())],
                    else_branch: vec![Node::Text(" Z ".to_string())],
                }],
            }]
        );
    }

    #[test]
    fn parses_a_chain_of_multiple_elseifs() {
        let nodes = parse("@if(a) X @elseif(b) Y @elseif(c) Z @else W @endif").unwrap();
        assert_eq!(
            nodes,
            vec![Node::If {
                cond: "a".to_string(),
                then_branch: vec![Node::Text(" X ".to_string())],
                else_branch: vec![Node::If {
                    cond: "b".to_string(),
                    then_branch: vec![Node::Text(" Y ".to_string())],
                    else_branch: vec![Node::If {
                        cond: "c".to_string(),
                        then_branch: vec![Node::Text(" Z ".to_string())],
                        else_branch: vec![Node::Text(" W ".to_string())],
                    }],
                }],
            }]
        );
    }

    #[test]
    fn parses_a_chain_of_multiple_elseifs_with_no_trailing_else() {
        let nodes = parse("@if(a) X @elseif(b) Y @elseif(c) Z @endif").unwrap();
        assert_eq!(
            nodes,
            vec![Node::If {
                cond: "a".to_string(),
                then_branch: vec![Node::Text(" X ".to_string())],
                else_branch: vec![Node::If {
                    cond: "b".to_string(),
                    then_branch: vec![Node::Text(" Y ".to_string())],
                    else_branch: vec![Node::If {
                        cond: "c".to_string(),
                        then_branch: vec![Node::Text(" Z ".to_string())],
                        else_branch: vec![],
                    }],
                }],
            }]
        );
    }

    #[test]
    fn stray_elseif_at_top_level_is_a_clear_error() {
        let err = parse("hi @elseif(x) there @endif").unwrap_err();
        assert!(err.to_string().contains("no matching opening directive"));
    }

    #[test]
    fn if_condition_handles_nested_parens_and_strings() {
        let nodes = parse(r#"@if(user.name == "a)b") x @endif"#).unwrap();
        let Node::If { cond, .. } = &nodes[0] else {
            panic!("expected If node");
        };
        assert_eq!(cond, r#"user.name == "a)b""#);
    }

    #[test]
    fn parses_foreach() {
        let nodes = parse("@foreach(post in posts){{ post }}@endforeach").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Foreach {
                binding: "post".to_string(),
                iter: "posts".to_string(),
                body: vec![Node::Interpolate {
                    expr: "post".to_string(),
                    escape: true
                }],
            }]
        );
    }

    #[test]
    fn parses_extends_section_yield() {
        let nodes = parse("@extends('layouts.app')@section('content')hi@endsection").unwrap();
        assert_eq!(
            nodes,
            vec![
                Node::Extends("layouts.app".to_string()),
                Node::Section {
                    name: "content".to_string(),
                    body: vec![Node::Text("hi".to_string())],
                },
            ]
        );

        let nodes = parse("@yield('content')").unwrap();
        assert_eq!(nodes, vec![Node::Yield("content".to_string())]);
    }

    #[test]
    fn parses_push_and_stack() {
        let nodes = parse("@push('scripts')<script></script>@endpush").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Push {
                name: "scripts".to_string(),
                body: vec![Node::Text("<script></script>".to_string())],
            }]
        );

        let nodes = parse("@stack('scripts')").unwrap();
        assert_eq!(nodes, vec![Node::Stack("scripts".to_string())]);
    }

    #[test]
    fn stray_endpush_without_a_matching_push_is_a_clear_error() {
        let err = parse("hi @endpush there").unwrap_err();
        assert!(err.to_string().contains("no matching opening directive"));
    }

    #[test]
    fn parses_csrf() {
        let nodes = parse("<form>@csrf</form>").unwrap();
        assert_eq!(
            nodes,
            vec![
                Node::Text("<form>".to_string()),
                Node::Csrf,
                Node::Text("</form>".to_string()),
            ]
        );
    }

    #[test]
    fn mismatched_closer_is_a_clear_error() {
        let err = parse("@if(x) hi @endforeach").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("@endif"), "message was: {message}");
        assert!(
            message.contains("found @endforeach"),
            "message was: {message}"
        );
    }

    #[test]
    fn unclosed_directive_is_a_clear_error() {
        let err = parse("@if(x) hi").unwrap_err();
        assert!(err.to_string().contains("unexpected end of template"));
    }

    #[test]
    fn stray_closer_at_top_level_is_a_clear_error() {
        let err = parse("hi @endif").unwrap_err();
        assert!(err.to_string().contains("no matching opening directive"));
    }

    #[test]
    fn parses_global_placeholder() {
        let nodes = parse("<title>@global(title)</title>").unwrap();
        assert_eq!(
            nodes,
            vec![
                Node::Text("<title>".to_string()),
                Node::Global {
                    name: "title".to_string(),
                    fallback: None,
                },
                Node::Text("</title>".to_string()),
            ]
        );
    }

    #[test]
    fn parses_global_placeholder_with_a_fallback() {
        let nodes = parse(r#"<title>@global(title, "Larust")</title>"#).unwrap();
        assert_eq!(
            nodes,
            vec![
                Node::Text("<title>".to_string()),
                Node::Global {
                    name: "title".to_string(),
                    fallback: Some("\"Larust\"".to_string()),
                },
                Node::Text("</title>".to_string()),
            ]
        );
    }

    #[test]
    fn global_fallback_can_be_an_arbitrary_expression_with_nested_parens_and_strings() {
        let nodes = parse(r#"@global(title, default_title(page, "(fallback)"))"#).unwrap();
        assert_eq!(
            nodes,
            vec![Node::Global {
                name: "title".to_string(),
                fallback: Some(r#"default_title(page, "(fallback)")"#.to_string()),
            }]
        );
    }

    #[test]
    fn missing_comma_or_close_paren_in_global_is_a_clear_error() {
        let err = parse("@global(title 'oops')").unwrap_err();
        assert!(
            err.to_string().contains("expected ',' or ')'"),
            "message was: {err}"
        );
    }

    #[test]
    fn parses_globals_block_with_multiple_assignments() {
        let nodes = parse(
            "@globals\n\
             title = \"My Page\"\n\
             canonical = \"https://example.com\"\n\
             @endglobals",
        )
        .unwrap();
        assert_eq!(
            nodes,
            vec![Node::Globals(vec![
                ("title".to_string(), "\"My Page\"".to_string()),
                (
                    "canonical".to_string(),
                    "\"https://example.com\"".to_string()
                ),
            ])]
        );
    }

    #[test]
    fn globals_block_skips_blank_lines() {
        let nodes = parse("@globals\n\ntitle = \"x\"\n\n@endglobals").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Globals(vec![(
                "title".to_string(),
                "\"x\"".to_string()
            )])]
        );
    }

    #[test]
    fn globals_assignment_with_equality_operator_in_expression() {
        let nodes = parse("@globals\nactive = state == \"open\"\n@endglobals").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Globals(vec![(
                "active".to_string(),
                "state == \"open\"".to_string()
            )])]
        );
    }

    #[test]
    fn globals_assignment_with_equals_sign_inside_string_literal() {
        let nodes = parse("@globals\nnote = \"a=b\"\n@endglobals").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Globals(vec![(
                "note".to_string(),
                "\"a=b\"".to_string()
            )])]
        );
    }

    #[test]
    fn malformed_globals_line_is_a_clear_error() {
        let err = parse("@globals\nnot an assignment\n@endglobals").unwrap_err();
        assert!(
            err.to_string().contains("expected `name = expression`"),
            "message was: {err}"
        );
    }

    #[test]
    fn unterminated_globals_block_is_a_clear_error() {
        let err = parse("@globals\ntitle = \"x\"").unwrap_err();
        assert!(err.to_string().contains("unterminated @globals block"));
    }

    #[test]
    fn globals_assignment_with_an_invalid_name_is_a_clear_error() {
        // A name containing a space (or any character `@global(name)`'s own
        // bare-identifier scanner wouldn't accept) must be rejected here
        // too — otherwise it silently registers under a key no
        // `@global(...)` placeholder could ever match, and the mismatch
        // would surface nowhere at all.
        let err = parse("@globals\nmy title = \"x\"\n@endglobals").unwrap_err();
        assert!(
            err.to_string().contains("invalid variable name"),
            "message was: {err}"
        );
    }

    #[test]
    fn stray_endglobals_at_top_level_is_a_clear_error() {
        let err = parse("hi @endglobals there").unwrap_err();
        assert!(err.to_string().contains("no matching opening directive"));
    }

    #[test]
    fn parses_live_with_no_props() {
        let nodes = parse("@live('search-box')").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Live {
                name: "search-box".to_string(),
                props: vec![],
            }]
        );
    }

    #[test]
    fn parses_live_with_props() {
        let nodes = parse(r#"@live('search-box', { query: "", limit: 10 })"#).unwrap();
        assert_eq!(
            nodes,
            vec![Node::Live {
                name: "search-box".to_string(),
                props: vec![
                    ("query".to_string(), "\"\"".to_string()),
                    ("limit".to_string(), "10".to_string()),
                ],
            }]
        );
    }

    #[test]
    fn live_props_handle_nested_parens_braces_brackets_and_strings() {
        let nodes = parse(
            r#"@live('widget', { items: vec![1, 2], meta: Foo { a: 1 }, label: default_label("(x)") })"#,
        )
        .unwrap();
        assert_eq!(
            nodes,
            vec![Node::Live {
                name: "widget".to_string(),
                props: vec![
                    ("items".to_string(), "vec![1, 2]".to_string()),
                    ("meta".to_string(), "Foo { a: 1 }".to_string()),
                    ("label".to_string(), r#"default_label("(x)")"#.to_string()),
                ],
            }]
        );
    }

    #[test]
    fn live_props_handle_a_trailing_comma() {
        let nodes = parse(r#"@live('widget', { query: "x", })"#).unwrap();
        assert_eq!(
            nodes,
            vec![Node::Live {
                name: "widget".to_string(),
                props: vec![("query".to_string(), "\"x\"".to_string())],
            }]
        );
    }

    #[test]
    fn live_with_empty_props_object() {
        let nodes = parse("@live('widget', {})").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Live {
                name: "widget".to_string(),
                props: vec![],
            }]
        );
    }

    #[test]
    fn missing_prop_colon_is_a_clear_error() {
        let err = parse("@live('widget', { query })").unwrap_err();
        assert!(
            err.to_string().contains("expected `name: expression`"),
            "message was: {err}"
        );
    }

    #[test]
    fn missing_prop_value_is_a_clear_error() {
        let err = parse("@live('widget', { query: })").unwrap_err();
        assert!(
            err.to_string().contains("expected an expression after ':'"),
            "message was: {err}"
        );
    }

    #[test]
    fn invalid_prop_name_is_a_clear_error() {
        let err = parse(r#"@live('widget', { "query": "x" })"#).unwrap_err();
        assert!(
            err.to_string().contains("invalid prop name"),
            "message was: {err}"
        );
    }

    #[test]
    fn unterminated_live_props_is_a_clear_error() {
        let err = parse("@live('widget', { query: \"x\"").unwrap_err();
        assert!(
            err.to_string().contains("unterminated '{' in @live"),
            "message was: {err}"
        );
    }

    #[test]
    fn missing_comma_or_close_paren_in_live_is_a_clear_error() {
        let err = parse("@live('widget' oops)").unwrap_err();
        assert!(
            err.to_string().contains("expected ',' or ')'"),
            "message was: {err}"
        );
    }

    #[test]
    fn parses_larustscripts() {
        let nodes = parse("<body>@larustscripts</body>").unwrap();
        assert_eq!(
            nodes,
            vec![
                Node::Text("<body>".to_string()),
                Node::LarustScripts,
                Node::Text("</body>".to_string()),
            ]
        );
    }

    #[test]
    fn parses_loadonce() {
        let nodes = parse("@loadonce<script></script>@endloadonce").unwrap();
        assert_eq!(
            nodes,
            vec![Node::LoadOnce(vec![Node::Text(
                "<script></script>".to_string()
            )])]
        );
    }

    #[test]
    fn loadonce_can_contain_ordinary_directives() {
        let nodes = parse("@loadonce@if(ok)yes@endif@endloadonce").unwrap();
        assert_eq!(
            nodes,
            vec![Node::LoadOnce(vec![Node::If {
                cond: "ok".to_string(),
                then_branch: vec![Node::Text("yes".to_string())],
                else_branch: vec![],
            }])]
        );
    }

    #[test]
    fn unclosed_loadonce_is_a_clear_error() {
        let err = parse("@loadonce<script></script>").unwrap_err();
        assert!(err.to_string().contains("unexpected end of template"));
    }

    #[test]
    fn stray_endloadonce_at_top_level_is_a_clear_error() {
        let err = parse("hi @endloadonce there").unwrap_err();
        assert!(err.to_string().contains("no matching opening directive"));
    }
}
