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
    "wire",
    "larustscripts",
    "loadonce",
    "endloadonce",
    "resource",
    "endresource",
    "live",
    "endlive",
    "code",
    "endcode",
    "vitex",
    "js",
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
    "endresource",
    "endlive",
    "endcode",
];

/// A directive that ended the block currently being parsed. `elseif_cond`
/// is only ever set when `tag == "elseif"` — its condition is consumed
/// right where the closer itself is recognized (see `read_closer`), since
/// `@elseif`'s condition logically belongs to whatever nested `@if` it
/// desugars into (see `parse_if_tail`), not to this closer signal.
/// `tag` borrows from `KEYWORDS` (always `'static`), not the source text —
/// no allocation for something as cheap and frequent as a block boundary.
///
/// `resource_tag_name` is only ever set when `tag == "endresourcetag"` — a
/// closing `</resource:name>` tag (see `parse_resource_tag`), the one
/// closer whose "which specific thing does this close" identity can't be
/// captured by `tag` alone, since (unlike every `@endXxx` directive) two
/// sibling `<resource:a>`/`<resource:b>` tags need their closers told
/// apart by name, not just by kind.
struct Closer {
    tag: &'static str,
    elseif_cond: Option<String>,
    resource_tag_name: Option<String>,
}

pub fn parse(source: &str) -> Result<Vec<Node>, ParseError> {
    let mut cursor = Cursor::new(source);
    let (nodes, closer) = parse_nodes(&mut cursor)?;
    if let Some(closer) = closer {
        let message = match &closer.resource_tag_name {
            Some(name) => {
                format!("unexpected </resource:{name}> with no matching <resource:{name}>")
            }
            None => format!(
                "unexpected @{} with no matching opening directive",
                closer.tag
            ),
        };
        return Err(ParseError::new(message));
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
    ResourceTagOpen,
    ResourceTagClose,
    WireTagOpen,
}

fn next_marker(s: &str) -> Option<(usize, MarkerKind)> {
    let at = find_next_at_directive(s).map(|(p, kw)| (p, MarkerKind::At(kw)));
    let raw = s.find("{!!").map(|p| (p, MarkerKind::Raw));
    let esc = s.find("{{").map(|p| (p, MarkerKind::Escaped));
    // Checked in this order (close before open) purely so the two `find`
    // calls are independent of each other's result — `</resource:` and
    // `<resource:` are distinct literal substrings (`</` vs `<r`), so
    // there's no actual ambiguity between them either way.
    let resource_close = s
        .find("</resource:")
        .map(|p| (p, MarkerKind::ResourceTagClose));
    let resource_open = s
        .find("<resource:")
        .map(|p| (p, MarkerKind::ResourceTagOpen));
    let wire_open = s.find("<wire:").map(|p| (p, MarkerKind::WireTagOpen));

    [at, raw, esc, resource_close, resource_open, wire_open]
        .into_iter()
        .flatten()
        .min_by_key(|(p, _)| *p)
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
        resource_tag_name: None,
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
                    MarkerKind::ResourceTagOpen => {
                        nodes.push(parse_resource_tag(cur)?);
                    }
                    MarkerKind::WireTagOpen => {
                        nodes.push(parse_wire_tag(cur)?);
                    }
                    MarkerKind::ResourceTagClose => {
                        cur.advance("</resource:".len());
                        let name = scan_tag_name(cur, "resource")?;
                        skip_ws(cur);
                        expect_char(cur, '>')?;
                        return Ok((
                            nodes,
                            Some(Closer {
                                tag: "endresourcetag",
                                elseif_cond: None,
                                resource_tag_name: Some(name),
                            }),
                        ));
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
                            "wire" => {
                                let (name, props) = parse_wire_args(cur)?;
                                nodes.push(Node::Wire { name, props });
                            }
                            "larustscripts" => {
                                nodes.push(Node::LarustScripts);
                            }
                            "loadonce" => {
                                let (body, closer) = parse_nodes(cur)?;
                                expect_closer(closer, "endloadonce")?;
                                nodes.push(Node::LoadOnce(body));
                            }
                            "resource" => {
                                let (name, props) = parse_resource_args(cur)?;
                                let (slot, closer) = parse_nodes(cur)?;
                                expect_closer(closer, "endresource")?;
                                nodes.push(Node::Resource { name, props, slot });
                            }
                            "live" => {
                                let channel = parse_paren_expr(cur)?;
                                let (body, closer) = parse_nodes(cur)?;
                                expect_closer(closer, "endlive")?;
                                nodes.push(Node::Live { channel, body });
                            }
                            "code" => nodes.push(Node::Code(parse_code_block(cur)?)),
                            "js" => {
                                let expr = parse_paren_expr(cur)?;
                                nodes.push(Node::Js(expr));
                            }
                            "vitex" => {
                                let entries = parse_vitex_args(cur)?;
                                nodes.push(Node::Vitex(entries));
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

/// Reads raw Rust statements until `@endcode`. Unlike normal template
/// bodies this is intentionally not parsed as markup: the macro validates
/// the statements with `syn` when it expands the template.
fn parse_code_block(cur: &mut Cursor) -> Result<String, ParseError> {
    let rest = cur.rest();
    let end = rest
        .find("@endcode")
        .ok_or_else(|| ParseError::new("unterminated @code block, expected @endcode"))?;
    let code = rest[..end].to_string();
    cur.advance(end + "@endcode".len());
    Ok(code)
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
            ..
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
/// `parse_wire_args` (`@wire('name', ...)`'s own name argument, which needs
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

/// Parses `@vitex(['path1', 'path2', ...])` — an array of quoted entry
/// paths, matching Laravel's own `@vite([...])` syntax exactly (so a
/// hand-converted template reads the same way the original did). Each
/// entry is a plain quoted string via [`parse_quoted_string`] — no
/// interpolation, no expression, same as every real `@vite(...)` call
/// this exists to mirror. A trailing comma before `]` is tolerated
/// (`['a', 'b',]`), matching how the array literal usually reads when
/// hand-formatted across multiple lines.
fn parse_vitex_args(cur: &mut Cursor) -> Result<Vec<String>, ParseError> {
    skip_ws(cur);
    expect_char(cur, '(')?;
    skip_ws(cur);
    expect_char(cur, '[')?;

    let mut entries = Vec::new();
    loop {
        skip_ws(cur);
        if cur.rest().starts_with(']') {
            cur.advance(1);
            break;
        }
        entries.push(parse_quoted_string(cur)?);
        skip_ws(cur);
        match cur.rest().chars().next() {
            Some(',') => cur.advance(1),
            Some(']') => {
                cur.advance(1);
                break;
            }
            _ => return Err(ParseError::new("expected ',' or ']' in @vitex([...])")),
        }
    }
    skip_ws(cur);
    expect_char(cur, ')')?;
    if entries.is_empty() {
        return Err(ParseError::new(
            "@vitex([...]) needs at least one entry path",
        ));
    }
    Ok(entries)
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

/// Parses `@wire('name')` or `@wire('name', { prop: expr, ... })`. The
/// component name reuses `parse_quoted_string`, the same convention
/// `@extends('...')` uses; the optional props object is a brace-delimited,
/// comma-separated `key: expr` list — see `parse_prop_entries`.
fn parse_wire_args(cur: &mut Cursor) -> Result<(String, Vec<(String, String)>), ParseError> {
    parse_name_and_props_args(cur, "wire")
}

/// Parses `@resource('name')` or `@resource('name', { prop: expr, ... })`
/// — the directive's opening arguments only; the `... @endresource` body
/// (the slot) is parsed separately by the caller via the ordinary
/// `parse_nodes` block-parsing path, same as `@section`/`@push`/`@loadonce`.
/// Identical grammar to `@wire(...)`'s own arguments — shares the same
/// scanner rather than duplicating it.
fn parse_resource_args(cur: &mut Cursor) -> Result<(String, Vec<(String, String)>), ParseError> {
    parse_name_and_props_args(cur, "resource")
}

/// Shared by `@wire(...)`/`@resource(...)`: `('name')` or
/// `('name', { prop: expr, ... })`, with `directive` only used to name the
/// directive in error messages.
fn parse_name_and_props_args(
    cur: &mut Cursor,
    directive: &str,
) -> Result<(String, Vec<(String, String)>), ParseError> {
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
            "expected ',' or ')' in @{directive}(...), found '{c}'"
        ))),
        None => Err(ParseError::new(format!(
            "expected ',' or ')' in @{directive}(...), found end of template"
        ))),
    }
}

/// Parses `<resource:name attr="literal" :attr2="expr" />` (self-closing,
/// empty slot) or `<resource:name ...>slot</resource:name>` (block form) —
/// an alternate, HTML-tag-flavored surface syntax for exactly the same
/// [`Node::Resource`] the `@resource('name', { ... }) ... @endresource`
/// directive (`parse_resource_args` above) produces. Not a distinct AST
/// concept: `resolve.rs` and `larust-macros`' codegen can't tell the two
/// spellings apart, so a template can freely mix both, and adding this
/// second spelling required zero changes outside this file.
///
/// Unprefixed attributes are literal string props — `title="Your profile."`
/// becomes exactly the prop this file already builds for `{ title: "Your
/// profile." }` (the raw attribute text re-escaped into a Rust string
/// literal by `literal_attr_to_rust_string`). A leading `:` marks an
/// attribute's value as a raw Rust expression instead — `:count="count"` is
/// `{ count: count }` — Blade's own `<x-alert :message="$message">`
/// convention. The cursor is positioned just past `<resource:` on entry.
fn parse_resource_tag(cur: &mut Cursor) -> Result<Node, ParseError> {
    cur.advance("<resource:".len());
    let name = scan_tag_name(cur, "resource")?;
    let props = parse_tag_attrs(cur, "resource")?;
    skip_ws(cur);

    if cur.rest().starts_with("/>") {
        cur.advance(2);
        return Ok(Node::Resource {
            name,
            props,
            slot: Vec::new(),
        });
    }

    expect_char(cur, '>')?;
    let (slot, closer) = parse_nodes(cur)?;
    match closer {
        Some(Closer {
            resource_tag_name: Some(found),
            ..
        }) if found == name => Ok(Node::Resource { name, props, slot }),
        // A closing tag with the *wrong* name (a copy-paste/rename slip)
        // is deliberately distinguished from "no closing tag at all" here
        // — both fall into this arm, but `unexpected_closer` renders each
        // found-value distinctly, so the error names exactly what was
        // actually found (a mismatched tag, an unrelated `@endXxx`
        // bubbling up from an unbalanced directive inside the slot, or
        // end of template).
        other => Err(unexpected_closer(other, &format!("</resource:{name}>"))),
    }
}

/// Parses `<wire:name attr="literal" :attr2="expr" />` — the HTML-tag-
/// flavored counterpart to `@wire('name', { ... })`, producing the
/// identical [`Node::Wire`]. **Always self-closing** — unlike
/// `<resource:...>`, `@wire(...)` has never had a body/slot concept at
/// all (a mounted component renders entirely from its own template), so
/// there's no block form to support, and a stray non-self-closing `>` is
/// a clear error rather than being silently accepted as "no slot". The
/// cursor is positioned just past `<wire:` on entry.
fn parse_wire_tag(cur: &mut Cursor) -> Result<Node, ParseError> {
    cur.advance("<wire:".len());
    let name = scan_tag_name(cur, "wire")?;
    let props = parse_tag_attrs(cur, "wire")?;
    skip_ws(cur);

    if cur.rest().starts_with("/>") {
        cur.advance(2);
        return Ok(Node::Wire { name, props });
    }

    match cur.rest().chars().next() {
        Some('>') => Err(ParseError::new(format!(
            "<wire:{name}> must be self-closing ('/>') — it has no closing tag or slot, \
             unlike <resource:...>"
        ))),
        Some(c) => Err(ParseError::new(format!(
            "expected '/>' to close <wire:{name}>, found '{c}'"
        ))),
        None => Err(ParseError::new(format!(
            "expected '/>' to close <wire:{name}>, found end of template"
        ))),
    }
}

/// Scans a tag's dotted name (`components.panel` after `<resource:`, or a
/// component name after `<wire:`) — any run of non-whitespace characters
/// up to `/`, `>`, or whitespace. Deliberately no character-class
/// validation, same "an invalid name surfaces later, not here" stance the
/// directive-syntax name arguments already take. `tag_prefix`
/// (`"resource"`/`"wire"`) is only used to phrase the error message.
fn scan_tag_name(cur: &mut Cursor, tag_prefix: &str) -> Result<String, ParseError> {
    let s = cur.rest();
    let end = s
        .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
        .unwrap_or(s.len());
    if end == 0 {
        return Err(ParseError::new(format!(
            "expected a name after '<{tag_prefix}:'"
        )));
    }
    let name = s[..end].to_string();
    cur.advance(end);
    Ok(name)
}

/// Parses zero or more `name="literal"` / `:name="expr"` attributes,
/// stopping (without consuming) at the tag's own closing `/>` or `>` —
/// shared by `<resource:...>` and `<wire:...>`, whose attribute grammar is
/// identical; `tag_prefix` is only used to phrase an error message.
fn parse_tag_attrs(
    cur: &mut Cursor,
    tag_prefix: &str,
) -> Result<Vec<(String, String)>, ParseError> {
    let mut props = Vec::new();
    loop {
        skip_ws(cur);
        match cur.rest().chars().next() {
            Some('/') | Some('>') | None => return Ok(props),
            _ => {}
        }

        let dynamic = cur.rest().starts_with(':');
        if dynamic {
            cur.advance(1);
        }

        let s = cur.rest();
        let end = s
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(s.len());
        if end == 0 {
            return Err(ParseError::new(format!(
                "expected an attribute name in <{tag_prefix}:...> tag"
            )));
        }
        let attr_name = s[..end].to_string();
        cur.advance(end);
        skip_ws(cur);
        expect_char(cur, '=')?;
        skip_ws(cur);
        let raw_value = parse_quoted_string(cur)?;

        let expr = if dynamic {
            raw_value
        } else {
            literal_attr_to_rust_string(&raw_value)
        };
        props.push((attr_name, expr));
    }
}

/// Wraps raw, unescaped attribute text in a valid Rust string literal —
/// `Your profile.` becomes `"Your profile."`, `Say "hi"` becomes `"Say
/// \"hi\""`. Only `"` and `\` need escaping; Rust string literals otherwise
/// accept any character (including a literal newline) as-is.
fn literal_attr_to_rust_string(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len() + 2);
    escaped.push('"');
    for c in raw.chars() {
        match c {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            _ => escaped.push(c),
        }
    }
    escaped.push('"');
    escaped
}

/// Parses the interior of a `@wire(..., { key: expr, key2: expr2 })` props
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

    let end = end.ok_or_else(|| ParseError::new("unterminated '{' in @wire(...) props"))?;
    let body = &s[..end];
    cur.advance(end + 1);

    split_prop_entries(body)
}

/// Splits a `@wire(...)` props body on top-level commas (honoring nested
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
            "expected `name: expression` in @wire(...) props, found: `{entry}`"
        ))
    })?;
    let name = entry[..split].trim().to_string();
    let expr = entry[split + 1..].trim().to_string();
    // Same character class `@global`/`@globals` already require of a
    // placeholder name — kept consistent rather than inventing a separate
    // rule for prop names.
    if !is_valid_global_name(&name) {
        return Err(ParseError::new(format!(
            "invalid prop name `{name}` in @wire(...) — only letters, digits, and underscores \
             are allowed"
        )));
    }
    if expr.is_empty() {
        return Err(ParseError::new(format!(
            "expected an expression after ':' in @wire(...) props, found: `{entry}`"
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
        Some(closer) => ParseError::new(format!(
            "expected {expected}, found {}",
            describe_closer(&closer)
        )),
        None => ParseError::new(format!("unexpected end of template, expected {expected}")),
    }
}

/// Renders a `Closer` the way it should read in an error message — a
/// resource-tag closer as `</resource:name>`, every other closer as
/// `@tag`, matching how each was actually spelled in the template.
fn describe_closer(closer: &Closer) -> String {
    match &closer.resource_tag_name {
        Some(name) => format!("</resource:{name}>"),
        None => format!("@{}", closer.tag),
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
    fn parses_foreach_with_a_keyed_tuple_binding() {
        let nodes =
            parse("@foreach((key, item) in items.iter().enumerate()){{ key }}@endforeach").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Foreach {
                binding: "(key, item)".to_string(),
                iter: "items.iter().enumerate()".to_string(),
                body: vec![Node::Interpolate {
                    expr: "key".to_string(),
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
    fn parses_wire_with_no_props() {
        let nodes = parse("@wire('search-box')").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Wire {
                name: "search-box".to_string(),
                props: vec![],
            }]
        );
    }

    #[test]
    fn parses_wire_with_props() {
        let nodes = parse(r#"@wire('search-box', { query: "", limit: 10 })"#).unwrap();
        assert_eq!(
            nodes,
            vec![Node::Wire {
                name: "search-box".to_string(),
                props: vec![
                    ("query".to_string(), "\"\"".to_string()),
                    ("limit".to_string(), "10".to_string()),
                ],
            }]
        );
    }

    #[test]
    fn wire_props_handle_nested_parens_braces_brackets_and_strings() {
        let nodes = parse(
            r#"@wire('widget', { items: vec![1, 2], meta: Foo { a: 1 }, label: default_label("(x)") })"#,
        )
        .unwrap();
        assert_eq!(
            nodes,
            vec![Node::Wire {
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
    fn wire_props_handle_a_trailing_comma() {
        let nodes = parse(r#"@wire('widget', { query: "x", })"#).unwrap();
        assert_eq!(
            nodes,
            vec![Node::Wire {
                name: "widget".to_string(),
                props: vec![("query".to_string(), "\"x\"".to_string())],
            }]
        );
    }

    #[test]
    fn wire_with_empty_props_object() {
        let nodes = parse("@wire('widget', {})").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Wire {
                name: "widget".to_string(),
                props: vec![],
            }]
        );
    }

    #[test]
    fn missing_prop_colon_is_a_clear_error() {
        let err = parse("@wire('widget', { query })").unwrap_err();
        assert!(
            err.to_string().contains("expected `name: expression`"),
            "message was: {err}"
        );
    }

    #[test]
    fn missing_prop_value_is_a_clear_error() {
        let err = parse("@wire('widget', { query: })").unwrap_err();
        assert!(
            err.to_string().contains("expected an expression after ':'"),
            "message was: {err}"
        );
    }

    #[test]
    fn invalid_prop_name_is_a_clear_error() {
        let err = parse(r#"@wire('widget', { "query": "x" })"#).unwrap_err();
        assert!(
            err.to_string().contains("invalid prop name"),
            "message was: {err}"
        );
    }

    #[test]
    fn unterminated_wire_props_is_a_clear_error() {
        let err = parse("@wire('widget', { query: \"x\"").unwrap_err();
        assert!(
            err.to_string().contains("unterminated '{' in @wire"),
            "message was: {err}"
        );
    }

    #[test]
    fn missing_comma_or_close_paren_in_wire_is_a_clear_error() {
        let err = parse("@wire('widget' oops)").unwrap_err();
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

    #[test]
    fn parses_resource_with_no_props_and_a_slot() {
        let nodes = parse("@resource('components.panel')<p>hi</p>@endresource").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Resource {
                name: "components.panel".to_string(),
                props: vec![],
                slot: vec![Node::Text("<p>hi</p>".to_string())],
            }]
        );
    }

    #[test]
    fn parses_resource_with_props_and_an_empty_slot() {
        let nodes =
            parse(r#"@resource('components.badge', { label: "New" })@endresource"#).unwrap();
        assert_eq!(
            nodes,
            vec![Node::Resource {
                name: "components.badge".to_string(),
                props: vec![("label".to_string(), "\"New\"".to_string())],
                slot: vec![],
            }]
        );
    }

    #[test]
    fn resource_slot_can_contain_ordinary_directives() {
        let nodes = parse("@resource('components.panel')@if(ok)yes@endif@endresource").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Resource {
                name: "components.panel".to_string(),
                props: vec![],
                slot: vec![Node::If {
                    cond: "ok".to_string(),
                    then_branch: vec![Node::Text("yes".to_string())],
                    else_branch: vec![],
                }],
            }]
        );
    }

    #[test]
    fn unclosed_resource_is_a_clear_error() {
        let err = parse("@resource('components.panel')<p>hi</p>").unwrap_err();
        assert!(err.to_string().contains("unexpected end of template"));
    }

    #[test]
    fn stray_endresource_at_top_level_is_a_clear_error() {
        let err = parse("hi @endresource there").unwrap_err();
        assert!(err.to_string().contains("no matching opening directive"));
    }

    #[test]
    fn missing_comma_or_close_paren_in_resource_is_a_clear_error() {
        let err = parse("@resource('panel' oops)@endresource").unwrap_err();
        assert!(
            err.to_string().contains("expected ',' or ')'"),
            "message was: {err}"
        );
    }

    #[test]
    fn parses_live_with_a_string_literal_channel() {
        let nodes = parse("@live('posts.count')<span>5</span>@endlive").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Live {
                channel: "'posts.count'".to_string(),
                body: vec![Node::Text("<span>5</span>".to_string())],
            }]
        );
    }

    #[test]
    fn live_channel_can_be_an_arbitrary_expression() {
        // Unlike `@wire`/`@resource`'s own `name` argument (a quoted
        // string only), `@live`'s channel is parsed the same way
        // `@if`/`@foreach` parse their own arguments — any expression, so
        // a channel can be scoped dynamically per-resource.
        let nodes = parse(r#"@live(format!("post.{}.comments", post.id))hi@endlive"#).unwrap();
        assert_eq!(
            nodes,
            vec![Node::Live {
                channel: r#"format!("post.{}.comments", post.id)"#.to_string(),
                body: vec![Node::Text("hi".to_string())],
            }]
        );
    }

    #[test]
    fn live_body_can_contain_ordinary_directives() {
        let nodes = parse("@live('c')@if(ok)yes@endif@endlive").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Live {
                channel: "'c'".to_string(),
                body: vec![Node::If {
                    cond: "ok".to_string(),
                    then_branch: vec![Node::Text("yes".to_string())],
                    else_branch: vec![],
                }],
            }]
        );
    }

    #[test]
    fn unclosed_live_is_a_clear_error() {
        let err = parse("@live('c')<p>hi</p>").unwrap_err();
        assert!(err.to_string().contains("unexpected end of template"));
    }

    #[test]
    fn stray_endlive_at_top_level_is_a_clear_error() {
        let err = parse("hi @endlive there").unwrap_err();
        assert!(err.to_string().contains("no matching opening directive"));
    }

    #[test]
    fn unterminated_paren_in_live_channel_is_a_clear_error() {
        let err = parse("@live('c'").unwrap_err();
        assert!(err.to_string().contains("unterminated '(' in directive"));
    }

    #[test]
    fn resource_tag_syntax_produces_the_same_node_the_directive_syntax_does() {
        let tag = parse(r#"<resource:components.badge label="New" />"#).unwrap();
        let directive =
            parse(r#"@resource('components.badge', { label: "New" })@endresource"#).unwrap();
        assert_eq!(tag, directive);
    }

    #[test]
    fn parses_resource_tag_self_closing_with_no_props() {
        let nodes = parse("<resource:components.panel/>").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Resource {
                name: "components.panel".to_string(),
                props: vec![],
                slot: vec![],
            }]
        );
    }

    #[test]
    fn parses_resource_tag_self_closing_with_literal_and_dynamic_props() {
        let nodes = parse(r#"<resource:components.badge label="New" :count="unread" />"#).unwrap();
        assert_eq!(
            nodes,
            vec![Node::Resource {
                name: "components.badge".to_string(),
                props: vec![
                    ("label".to_string(), "\"New\"".to_string()),
                    ("count".to_string(), "unread".to_string()),
                ],
                slot: vec![],
            }]
        );
    }

    #[test]
    fn parses_resource_tag_block_form_with_a_slot() {
        let nodes =
            parse(r#"<resource:components.panel title="Hi"><p>hi</p></resource:components.panel>"#)
                .unwrap();
        assert_eq!(
            nodes,
            vec![Node::Resource {
                name: "components.panel".to_string(),
                props: vec![("title".to_string(), "\"Hi\"".to_string())],
                slot: vec![Node::Text("<p>hi</p>".to_string())],
            }]
        );
    }

    #[test]
    fn resource_tag_literal_attribute_with_a_double_quote_is_escaped() {
        let nodes = parse(r#"<resource:components.badge label='Say "hi"' />"#).unwrap();
        assert_eq!(
            nodes,
            vec![Node::Resource {
                name: "components.badge".to_string(),
                props: vec![("label".to_string(), "\"Say \\\"hi\\\"\"".to_string())],
                slot: vec![],
            }]
        );
    }

    #[test]
    fn resource_tag_slot_can_contain_ordinary_directives() {
        let nodes =
            parse("<resource:components.panel>@if(ok)yes@endif</resource:components.panel>")
                .unwrap();
        assert_eq!(
            nodes,
            vec![Node::Resource {
                name: "components.panel".to_string(),
                props: vec![],
                slot: vec![Node::If {
                    cond: "ok".to_string(),
                    then_branch: vec![Node::Text("yes".to_string())],
                    else_branch: vec![],
                }],
            }]
        );
    }

    #[test]
    fn nested_resource_tags_parse_correctly() {
        let nodes = parse("<resource:a><resource:b>x</resource:b></resource:a>").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Resource {
                name: "a".to_string(),
                props: vec![],
                slot: vec![Node::Resource {
                    name: "b".to_string(),
                    props: vec![],
                    slot: vec![Node::Text("x".to_string())],
                }],
            }]
        );
    }

    #[test]
    fn resource_tag_name_mismatch_between_open_and_close_is_a_clear_error() {
        let err = parse("<resource:a>hi</resource:b>").unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("expected </resource:a>"),
            "message was: {message}"
        );
        assert!(
            message.contains("found </resource:b>"),
            "message was: {message}"
        );
    }

    #[test]
    fn unclosed_resource_tag_is_a_clear_error() {
        let err = parse("<resource:a><p>hi</p>").unwrap_err();
        assert!(err.to_string().contains("unexpected end of template"));
    }

    #[test]
    fn stray_closing_resource_tag_at_top_level_is_a_clear_error() {
        let err = parse("hi </resource:a> there").unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("no matching <resource:a>"),
            "message was: {message}"
        );
    }

    #[test]
    fn resource_tag_with_no_name_is_a_clear_error() {
        let err = parse("<resource:/>").unwrap_err();
        assert!(
            err.to_string()
                .contains("expected a name after '<resource:'"),
            "message was: {err}"
        );
    }

    #[test]
    fn wire_tag_syntax_produces_the_same_node_the_directive_syntax_does() {
        let tag = parse(r#"<wire:search-box query="" :limit="10" />"#).unwrap();
        let directive = parse(r#"@wire('search-box', { query: "", limit: 10 })"#).unwrap();
        assert_eq!(tag, directive);
    }

    #[test]
    fn parses_wire_tag_with_no_props() {
        let nodes = parse("<wire:search-box/>").unwrap();
        assert_eq!(
            nodes,
            vec![Node::Wire {
                name: "search-box".to_string(),
                props: vec![],
            }]
        );
    }

    #[test]
    fn parses_wire_tag_with_literal_and_dynamic_props() {
        let nodes = parse(r#"<wire:post-form title="Untitled" :post_id="post.id" />"#).unwrap();
        assert_eq!(
            nodes,
            vec![Node::Wire {
                name: "post-form".to_string(),
                props: vec![
                    ("title".to_string(), "\"Untitled\"".to_string()),
                    ("post_id".to_string(), "post.id".to_string()),
                ],
            }]
        );
    }

    #[test]
    fn wire_tag_without_self_close_is_a_clear_error() {
        let err = parse("<wire:search-box>").unwrap_err();
        assert!(
            err.to_string().contains("must be self-closing"),
            "message was: {err}"
        );
    }

    #[test]
    fn wire_tag_with_no_name_is_a_clear_error() {
        let err = parse("<wire:/>").unwrap_err();
        assert!(
            err.to_string().contains("expected a name after '<wire:'"),
            "message was: {err}"
        );
    }

    #[test]
    fn wire_tag_unterminated_is_a_clear_error() {
        let err = parse("<wire:search-box").unwrap_err();
        assert!(
            err.to_string()
                .contains("expected '/>' to close <wire:search-box>"),
            "message was: {err}"
        );
    }

    #[test]
    fn parses_trusted_rust_code_block() {
        assert_eq!(
            parse("@code let label = \"Hello\"; @endcode{{ label }}").unwrap(),
            vec![
                Node::Code(" let label = \"Hello\"; ".to_string()),
                Node::Interpolate {
                    expr: "label".to_string(),
                    escape: true,
                },
            ]
        );
    }

    #[test]
    fn parses_a_code_block_immediately_followed_by_a_raw_interpolation() {
        // A `@code` block immediately followed — no whitespace — by a
        // raw (unescaped) interpolation splicing its own result in,
        // rather than the escaped `{{ }}` form `parses_trusted_rust_code_
        // block` above already covers.
        assert_eq!(
            parse(
                "@code let __vitex_tags = larust_support::vitex::tags(&[\"a\"]); @endcode{!! __vitex_tags !!}"
            )
            .unwrap(),
            vec![
                Node::Code(
                    " let __vitex_tags = larust_support::vitex::tags(&[\"a\"]); ".to_string()
                ),
                Node::Interpolate {
                    expr: "__vitex_tags".to_string(),
                    escape: false,
                },
            ]
        );
    }

    #[test]
    fn parses_vitex_with_multiple_entries() {
        // Real source: `components/layouts/app.blade.xr`'s translated
        // `@vite(['resources/css/app.min.css', 'resources/js/app.min.js'])`
        // — `@vitex` mirrors that exact array-of-paths syntax.
        assert_eq!(
            parse("@vitex(['resources/css/app.min.css', 'resources/js/app.min.js'])").unwrap(),
            vec![Node::Vitex(vec![
                "resources/css/app.min.css".to_string(),
                "resources/js/app.min.js".to_string(),
            ])]
        );
    }

    #[test]
    fn parses_js_with_a_simple_expression() {
        let nodes = parse("<script>const post = @js(post);</script>").unwrap();
        assert_eq!(
            nodes,
            vec![
                Node::Text("<script>const post = ".to_string()),
                Node::Js("post".to_string()),
                Node::Text(";</script>".to_string()),
            ]
        );
    }

    #[test]
    fn js_expression_can_be_arbitrary_like_live_channel() {
        // `@js(...)` takes any expression, the same way `@live(...)`'s
        // channel argument does (`parse_paren_expr`, not the quoted-string-
        // only argument `@wire`/`@resource` take).
        let nodes = parse(r#"@js(serde_json::json!({"id": post.id}))"#).unwrap();
        assert_eq!(
            nodes,
            vec![Node::Js(
                r#"serde_json::json!({"id": post.id})"#.to_string()
            )]
        );
    }

    #[test]
    fn unterminated_paren_in_js_expression_is_a_clear_error() {
        let err = parse("@js(post").unwrap_err();
        assert!(err.to_string().contains("unterminated '(' in directive"));
    }

    #[test]
    fn parses_vitex_with_a_single_double_quoted_entry() {
        assert_eq!(
            parse("@vitex([\"resources/js/app.js\"])").unwrap(),
            vec![Node::Vitex(vec!["resources/js/app.js".to_string()])]
        );
    }

    #[test]
    fn parses_vitex_tolerating_a_trailing_comma() {
        assert_eq!(
            parse("@vitex(['a', 'b',])").unwrap(),
            vec![Node::Vitex(vec!["a".to_string(), "b".to_string()])]
        );
    }

    #[test]
    fn rejects_vitex_with_an_empty_array() {
        let err = parse("@vitex([])").unwrap_err();
        assert!(
            err.to_string().contains("at least one entry"),
            "message was: {err}"
        );
    }

    #[test]
    fn rejects_vitex_missing_the_array_brackets() {
        let err = parse("@vitex('resources/js/app.js')").unwrap_err();
        assert!(err.to_string().contains('['), "message was: {err}");
    }
}
