use crate::ast::{GlobalEntry, Node};
use crate::error::ParseError;
use std::collections::{HashMap, HashSet};

/// `resolve_with_context`'s own `@push`/`@globals` collections, gathered
/// alongside the resolved node list — see that function's own doc comment
/// for why callers need both, not just the flat node list `resolve`
/// alone returns.
pub type PushesAndGlobals = (HashMap<String, Vec<Node>>, HashMap<String, GlobalEntry>);

/// Resolves `@extends`/`@section`/`@yield`/`@push`/`@stack` composition
/// into a single, flat node list ready for codegen.
///
/// `load` fetches and parses another template by name (e.g. `"layouts.app"`)
/// — called recursively, so a layout chain (`page` extends `app` extends
/// `base`) resolves correctly, not just one level.
pub fn resolve(
    nodes: Vec<Node>,
    load: &mut impl FnMut(&str) -> Result<Vec<Node>, ParseError>,
) -> Result<Vec<Node>, ParseError> {
    let (nodes, _) = resolve_with_context(nodes, load)?;
    Ok(nodes)
}

/// Same as [`resolve`], but also returns the whole-tree `@push`/`@globals`
/// collections it gathered along the way — needed by `larust-macros`,
/// which loads and codegens each `<resource:...>` tag's own named template
/// *separately* from this call (see `Node::Resource`'s own doc comment in
/// `ast.rs`), and must apply [`substitute_stacks`]/[`substitute_globals`]
/// to that freshly-loaded content itself using these same maps — otherwise
/// a `@push`/`@stack` pair split across a resource-tag boundary (the
/// single most common shape in practice: a shared `components.layouts.app`
/// providing `@stack('head')`, included via `<resource:...>` from every
/// page, with pages pushing their own per-page `<link>`/`<meta>` tags into
/// it) would never connect — `@stack('head')` sitting inside that
/// separately-loaded layout file is invisible to *this* call's own
/// `substitute_stacks`, which only ever walks the nodes passed in as
/// `nodes` here.
pub fn resolve_with_context(
    nodes: Vec<Node>,
    load: &mut impl FnMut(&str) -> Result<Vec<Node>, ParseError>,
) -> Result<(Vec<Node>, PushesAndGlobals), ParseError> {
    let mut pushes: HashMap<String, Vec<Node>> = HashMap::new();
    let mut globals: HashMap<String, GlobalEntry> = HashMap::new();
    let resolved = resolve_inner(nodes, load, &mut HashSet::new(), &mut pushes, &mut globals)?;
    let resolved = substitute_stacks(resolved, &pushes);
    let resolved = substitute_globals(resolved, &globals);
    Ok((resolved, (pushes, globals)))
}

fn resolve_inner(
    nodes: Vec<Node>,
    load: &mut impl FnMut(&str) -> Result<Vec<Node>, ParseError>,
    seen: &mut HashSet<String>,
    pushes: &mut HashMap<String, Vec<Node>>,
    globals: &mut HashMap<String, GlobalEntry>,
) -> Result<Vec<Node>, ParseError> {
    // Collected from *every* level of the chain, before anything else —
    // `@push`/`@stack` are resolved as a wholly separate pass from
    // `@section`/`@yield` (see `resolve()`), specifically so a `@stack` in
    // the base-most layout can see pushes contributed by *every* level of
    // the chain (child, its parent, its parent's parent, ...), not just
    // whichever single level's `substitute_yields` call happened to reach
    // it first. Order: child-most level's own pushes first (this call
    // runs before recursing into the parent below), each level's own
    // multiple `@push`es to the same name in their own source order.
    collect_pushes(&nodes, pushes, load)?;
    // Same whole-chain-first shape as pushes, for the same reason — plus
    // one more: `@section`/`@yield`'s per-level eager `substitute_yields`
    // would let an indifferent *middle* layout blank a `@global` before a
    // leaf page's `@globals` ever reaches it (see `docs/MACROS.md`).
    // `collect_globals` (child-most first, first-write-wins in `globals`)
    // gives the page precedence over any ancestor layout setting the same
    // name, and lets it reach through layouts that don't touch that name
    // at all.
    collect_globals(&nodes, globals, load)?;

    let extends_name = nodes.iter().find_map(|n| match n {
        Node::Extends(name) => Some(name.clone()),
        _ => None,
    });

    let Some(parent_name) = extends_name else {
        return Ok(nodes);
    };

    // Without this, a template that (directly or via a longer chain)
    // extends itself recurses without bound — a stack overflow inside
    // rustc during macro expansion, not a clean diagnostic.
    if !seen.insert(parent_name.clone()) {
        return Err(ParseError::new(format!(
            "cycle detected in @extends chain (revisited `{parent_name}`)"
        )));
    }

    let mut sections = HashMap::new();
    for node in nodes {
        if let Node::Section { name, body } = node {
            sections.insert(name, body);
        }
    }

    let parent_nodes = load(&parent_name)?;
    let resolved_parent = resolve_inner(parent_nodes, load, seen, pushes, globals)?;
    Ok(substitute_yields(resolved_parent, &sections))
}

/// Recursively finds every `@push` in `nodes` (including inside
/// `@if`/`@foreach`/`@section` bodies, and inside another `@push`'s own
/// body) and appends its body to `pushes` under that push's name —
/// accumulating, not overwriting, since that's the whole point of a stack
/// versus a section.
///
/// Rejects a `@push` found inside a `@foreach`: `@push`/`@stack` here are
/// resolved once, statically, at macro-expansion time — there's no
/// per-iteration runtime step the way Laravel's own imperative,
/// output-buffered Blade compiler has. A push inside a loop would either
/// silently render its content exactly once instead of once per item, or
/// (if it references the loop variable at all) fail to compile with a
/// confusing "cannot find value" error pointing at generated code, not the
/// template. Both are worse than refusing it outright with a clear reason.
///
/// `Node::Resource { name, slot, .. }` recurses into *both* `slot` (the
/// caller-side content captured between `<resource:name>...</resource:name>`,
/// already part of `nodes`) *and* `load(name)` — the resource's own named
/// template, loaded fresh here purely to scan it for `@push`. That second
/// load is necessary (not merely "for consistency"): the resource's own
/// body is never otherwise part of any `resolve()` call's `nodes` — it's
/// loaded a second time, independently, by `larust-macros` at codegen time
/// for the actual rendering — so this is the *only* place a `@push` sitting
/// inside a resource file (rather than at its call site) is ever seen by
/// this collection pass. Naturally recursive for a resource file that
/// itself includes further `<resource:...>` tags.
fn collect_pushes(
    nodes: &[Node],
    pushes: &mut HashMap<String, Vec<Node>>,
    load: &mut impl FnMut(&str) -> Result<Vec<Node>, ParseError>,
) -> Result<(), ParseError> {
    for node in nodes {
        match node {
            Node::Push { name, body } => {
                pushes.entry(name.clone()).or_default().extend(body.clone());
                collect_pushes(body, pushes, load)?;
            }
            Node::If {
                then_branch,
                else_branch,
                ..
            }
            | Node::Can {
                then_branch,
                else_branch,
                ..
            }
            | Node::Role {
                then_branch,
                else_branch,
                ..
            } => {
                collect_pushes(then_branch, pushes, load)?;
                collect_pushes(else_branch, pushes, load)?;
            }
            Node::Foreach { body, .. } => {
                if contains_push(body) {
                    return Err(ParseError::new(
                        "`@push` inside `@foreach` isn't supported — pushed content is \
                         resolved once at compile time, not once per loop iteration, so a \
                         per-item push would silently render the wrong thing. Build the \
                         string yourself inside the loop instead.",
                    ));
                }
                collect_pushes(body, pushes, load)?;
            }
            Node::Section { body, .. }
            | Node::LoadOnce(body)
            | Node::Live { body, .. }
            | Node::Spa(body) => collect_pushes(body, pushes, load)?,
            Node::Resource { name, slot, .. } => {
                collect_pushes(slot, pushes, load)?;
                let resource_nodes = load(name)?;
                collect_pushes(&resource_nodes, pushes, load)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether `@push` appears anywhere in `nodes`, including nested inside
/// `@if`/`@foreach`/`@section`/another `@push` — used only to produce
/// `collect_pushes`'s "`@push` inside `@foreach`" error eagerly, before
/// walking (and partially registering) the offending subtree.
fn contains_push(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Push { .. } => true,
        Node::If {
            then_branch,
            else_branch,
            ..
        }
        | Node::Can {
            then_branch,
            else_branch,
            ..
        }
        | Node::Role {
            then_branch,
            else_branch,
            ..
        } => contains_push(then_branch) || contains_push(else_branch),
        Node::Foreach { body, .. }
        | Node::Section { body, .. }
        | Node::LoadOnce(body)
        | Node::Resource { slot: body, .. }
        | Node::Live { body, .. }
        | Node::Spa(body) => contains_push(body),
        _ => false,
    })
}

/// Recursively finds every `@globals` block in `nodes` (including inside
/// `@foreach`/`@section`/`@push` bodies — but **not** `@if`/`@elseif`/
/// `@else`, which is a compile error; see the `Node::If` arm below) and
/// merges its `name = expr` entries into `globals`.
///
/// Two-phase, deliberately: first walk `nodes` into a *local* map, where a
/// later assignment to the same name overwrites an earlier one within this
/// same walk (ordinary sequential-assignment shadowing, like `let x = 1;
/// let x = 2;`). Then merge that local map into the shared `globals` map
/// via `entry(...).or_insert(...)` — only if the name is absent — so a
/// value already collected from a more child-ward level (this function
/// runs child-most-first, see `resolve_inner`) is never overwritten by an
/// ancestor layout setting the same name. That's what makes a page's
/// `@globals` win over a layout's `@globals` for the same name, not the
/// other way around.
fn collect_globals(
    nodes: &[Node],
    globals: &mut HashMap<String, GlobalEntry>,
    load: &mut impl FnMut(&str) -> Result<Vec<Node>, ParseError>,
) -> Result<(), ParseError> {
    let mut local = HashMap::new();
    collect_globals_into(nodes, &mut local, load)?;
    for (name, entry) in local {
        globals.entry(name).or_insert(entry);
    }
    Ok(())
}

/// See `collect_pushes`'s own doc comment on the `Node::Resource` arm —
/// same reasoning applies here: a resource's own named template is loaded
/// a second time, independently of its call site's `slot`, purely to scan
/// it for `@globals` that would otherwise never be seen by any `resolve()`
/// call at all.
fn collect_globals_into(
    nodes: &[Node],
    local: &mut HashMap<String, GlobalEntry>,
    load: &mut impl FnMut(&str) -> Result<Vec<Node>, ParseError>,
) -> Result<(), ParseError> {
    for node in nodes {
        match node {
            Node::Globals(entries) => {
                for entry in entries {
                    local.insert(entry.name.clone(), entry.clone());
                }
            }
            // Both branches would otherwise be walked unconditionally —
            // `@if`'s runtime condition has no bearing on this compile-time
            // collection pass at all, so whichever branch happens to be
            // visited last would silently win regardless of which branch
            // the condition actually selects at render time. Same
            // "resolved once, can't express per-branch semantics" reasoning
            // as the `@foreach` rejection below, just for a different
            // directive. If neither branch has a `@globals` at all, there's
            // nothing to collect and nothing to reject — falls through to
            // the catch-all `_ => {}` below.
            Node::If {
                then_branch,
                else_branch,
                ..
            } if contains_globals(then_branch) || contains_globals(else_branch) => {
                return Err(ParseError::new(
                    "`@globals` inside `@if`/`@elseif`/`@else` isn't supported — global \
                     overrides are resolved once at compile time, from every branch \
                     unconditionally, not based on which branch the condition actually \
                     selects at render time, so the result would silently ignore the \
                     condition. Set the global unconditionally, or compute the value with a \
                     conditional expression instead (e.g. \
                     `title = if some_cond { \"A\" } else { \"B\" }`).",
                ));
            }
            Node::Can {
                then_branch,
                else_branch,
                ..
            } if contains_globals(then_branch) || contains_globals(else_branch) => {
                return Err(ParseError::new(
                    "`@globals` inside `@can`/`@else` isn't supported — same reason as inside \
                     `@if`: global overrides are resolved once at compile time, from every \
                     branch unconditionally, not based on whether the current user actually \
                     has the permission at render time.",
                ));
            }
            Node::Role {
                then_branch,
                else_branch,
                ..
            } if contains_globals(then_branch) || contains_globals(else_branch) => {
                return Err(ParseError::new(
                    "`@globals` inside `@role`/`@else` isn't supported — same reason as \
                     inside `@if`: global overrides are resolved once at compile time, from \
                     every branch unconditionally, not based on whether the current user \
                     actually has the role at render time.",
                ));
            }
            Node::Foreach { body, .. } => {
                if contains_globals(body) {
                    return Err(ParseError::new(
                        "`@globals` inside `@foreach` isn't supported — global overrides are \
                         resolved once at compile time, not once per loop iteration, so a \
                         per-item value has no coherent meaning.",
                    ));
                }
                collect_globals_into(body, local, load)?;
            }
            Node::Section { body, .. }
            | Node::Push { body, .. }
            | Node::LoadOnce(body)
            | Node::Live { body, .. }
            | Node::Spa(body) => {
                collect_globals_into(body, local, load)?;
            }
            Node::Resource { name, slot, .. } => {
                collect_globals_into(slot, local, load)?;
                let resource_nodes = load(name)?;
                collect_globals_into(&resource_nodes, local, load)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether `@globals` appears anywhere in `nodes`, including nested inside
/// `@if`/`@foreach`/`@section`/`@push` — used to produce `collect_globals`'s
/// "`@globals` inside `@foreach`" and "`@globals` inside `@if`" errors
/// eagerly, before walking (and, for `@foreach`, partially registering) the
/// offending subtree.
fn contains_globals(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Globals(_) => true,
        Node::If {
            then_branch,
            else_branch,
            ..
        }
        | Node::Can {
            then_branch,
            else_branch,
            ..
        }
        | Node::Role {
            then_branch,
            else_branch,
            ..
        } => contains_globals(then_branch) || contains_globals(else_branch),
        Node::Foreach { body, .. }
        | Node::Section { body, .. }
        | Node::Push { body, .. }
        | Node::LoadOnce(body)
        | Node::Resource { slot: body, .. }
        | Node::Live { body, .. }
        | Node::Spa(body) => contains_globals(body),
        _ => false,
    })
}

/// Mirrors `substitute_stacks` below, but for `@global`: walks the fully
/// resolved tree, replacing `Node::Global { name, fallback }` with a real
/// `Node::Interpolate` sourced from whichever `@globals` block (anywhere in
/// the chain) provided that name, falling back to `fallback` if none did,
/// or nothing if there's no fallback either — same "unset becomes empty"
/// convention as an unset `@stack`. A `persist`-flagged entry substitutes
/// to `Node::PersistGlobal` instead — its value isn't known until request
/// time, so it can't collapse to a literal `Interpolate` the way every
/// other entry does; see that node's own doc comment.
pub fn substitute_globals(nodes: Vec<Node>, globals: &HashMap<String, GlobalEntry>) -> Vec<Node> {
    nodes
        .into_iter()
        .flat_map(|node| -> Vec<Node> {
            match node {
                Node::Global { name, fallback } => match globals.get(&name) {
                    Some(entry) if entry.persist => vec![Node::PersistGlobal {
                        cookie_name: name,
                        fallback_expr: entry.expr.clone(),
                    }],
                    Some(entry) => vec![Node::Interpolate {
                        expr: entry.expr.clone(),
                        escape: true,
                    }],
                    None => match fallback {
                        Some(expr) => vec![Node::Interpolate { expr, escape: true }],
                        None => vec![],
                    },
                },
                Node::If {
                    cond,
                    then_branch,
                    else_branch,
                } => vec![Node::If {
                    cond,
                    then_branch: substitute_globals(then_branch, globals),
                    else_branch: substitute_globals(else_branch, globals),
                }],
                Node::Can {
                    permission,
                    then_branch,
                    else_branch,
                } => vec![Node::Can {
                    permission,
                    then_branch: substitute_globals(then_branch, globals),
                    else_branch: substitute_globals(else_branch, globals),
                }],
                Node::Role {
                    role,
                    then_branch,
                    else_branch,
                } => vec![Node::Role {
                    role,
                    then_branch: substitute_globals(then_branch, globals),
                    else_branch: substitute_globals(else_branch, globals),
                }],
                Node::Foreach {
                    binding,
                    iter,
                    body,
                } => vec![Node::Foreach {
                    binding,
                    iter,
                    body: substitute_globals(body, globals),
                }],
                Node::Section { name, body } => vec![Node::Section {
                    name,
                    body: substitute_globals(body, globals),
                }],
                Node::LoadOnce(body) => vec![Node::LoadOnce(substitute_globals(body, globals))],
                Node::Spa(body) => vec![Node::Spa(substitute_globals(body, globals))],
                Node::Resource { name, props, slot } => vec![Node::Resource {
                    name,
                    props,
                    slot: substitute_globals(slot, globals),
                }],
                Node::Live { channel, body } => vec![Node::Live {
                    channel,
                    body: substitute_globals(body, globals),
                }],
                other => vec![other],
            }
        })
        .collect()
}

/// Mirrors `substitute_yields` below, but for `@stack` — kept as a
/// separate pass (run once, after the entire `@extends` chain is already
/// section/yield-resolved) rather than folded into `substitute_yields`
/// itself, since `pushes` needs contributions from *every* level of the
/// chain before any `@stack` can be substituted correctly (see
/// `resolve_inner`'s doc comment on `collect_pushes`).
pub fn substitute_stacks(nodes: Vec<Node>, pushes: &HashMap<String, Vec<Node>>) -> Vec<Node> {
    nodes
        .into_iter()
        .flat_map(|node| -> Vec<Node> {
            match node {
                Node::Stack(name) => pushes.get(&name).cloned().unwrap_or_default(),
                Node::If {
                    cond,
                    then_branch,
                    else_branch,
                } => vec![Node::If {
                    cond,
                    then_branch: substitute_stacks(then_branch, pushes),
                    else_branch: substitute_stacks(else_branch, pushes),
                }],
                Node::Can {
                    permission,
                    then_branch,
                    else_branch,
                } => vec![Node::Can {
                    permission,
                    then_branch: substitute_stacks(then_branch, pushes),
                    else_branch: substitute_stacks(else_branch, pushes),
                }],
                Node::Role {
                    role,
                    then_branch,
                    else_branch,
                } => vec![Node::Role {
                    role,
                    then_branch: substitute_stacks(then_branch, pushes),
                    else_branch: substitute_stacks(else_branch, pushes),
                }],
                Node::Foreach {
                    binding,
                    iter,
                    body,
                } => vec![Node::Foreach {
                    binding,
                    iter,
                    body: substitute_stacks(body, pushes),
                }],
                Node::Section { name, body } => vec![Node::Section {
                    name,
                    body: substitute_stacks(body, pushes),
                }],
                Node::LoadOnce(body) => vec![Node::LoadOnce(substitute_stacks(body, pushes))],
                Node::Spa(body) => vec![Node::Spa(substitute_stacks(body, pushes))],
                Node::Resource { name, props, slot } => vec![Node::Resource {
                    name,
                    props,
                    slot: substitute_stacks(slot, pushes),
                }],
                Node::Live { channel, body } => vec![Node::Live {
                    channel,
                    body: substitute_stacks(body, pushes),
                }],
                other => vec![other],
            }
        })
        .collect()
}

fn substitute_yields(nodes: Vec<Node>, sections: &HashMap<String, Vec<Node>>) -> Vec<Node> {
    nodes
        .into_iter()
        .flat_map(|node| -> Vec<Node> {
            match node {
                Node::Yield(name) => sections.get(&name).cloned().unwrap_or_default(),
                Node::If {
                    cond,
                    then_branch,
                    else_branch,
                } => vec![Node::If {
                    cond,
                    then_branch: substitute_yields(then_branch, sections),
                    else_branch: substitute_yields(else_branch, sections),
                }],
                Node::Can {
                    permission,
                    then_branch,
                    else_branch,
                } => vec![Node::Can {
                    permission,
                    then_branch: substitute_yields(then_branch, sections),
                    else_branch: substitute_yields(else_branch, sections),
                }],
                Node::Role {
                    role,
                    then_branch,
                    else_branch,
                } => vec![Node::Role {
                    role,
                    then_branch: substitute_yields(then_branch, sections),
                    else_branch: substitute_yields(else_branch, sections),
                }],
                Node::Foreach {
                    binding,
                    iter,
                    body,
                } => vec![Node::Foreach {
                    binding,
                    iter,
                    body: substitute_yields(body, sections),
                }],
                Node::Section { name, body } => vec![Node::Section {
                    name,
                    body: substitute_yields(body, sections),
                }],
                Node::LoadOnce(body) => vec![Node::LoadOnce(substitute_yields(body, sections))],
                Node::Spa(body) => vec![Node::Spa(substitute_yields(body, sections))],
                Node::Resource { name, props, slot } => vec![Node::Resource {
                    name,
                    props,
                    slot: substitute_yields(slot, sections),
                }],
                Node::Live { channel, body } => vec![Node::Live {
                    channel,
                    body: substitute_yields(body, sections),
                }],
                other => vec![other],
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn substitutes_yield_with_matching_section() {
        let layout = parse("<body>@yield('content')</body>").unwrap();
        let child = parse("@extends('layout')@section('content')hi@endsection").unwrap();

        let resolved = resolve(child, &mut |name| {
            assert_eq!(name, "layout");
            Ok(layout.clone())
        })
        .unwrap();

        assert_eq!(
            resolved,
            vec![
                Node::Text("<body>".to_string()),
                Node::Text("hi".to_string()),
                Node::Text("</body>".to_string()),
            ]
        );
    }

    #[test]
    fn yield_with_no_matching_section_becomes_empty() {
        let layout = parse("<body>@yield('content')</body>").unwrap();
        let child = parse("@extends('layout')").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        assert_eq!(
            resolved,
            vec![
                Node::Text("<body>".to_string()),
                Node::Text("</body>".to_string()),
            ]
        );
    }

    #[test]
    fn no_extends_returns_nodes_unchanged() {
        let nodes = parse("just text").unwrap();
        let resolved = resolve(nodes.clone(), &mut |_| unreachable!()).unwrap();
        assert_eq!(resolved, nodes);
    }

    #[test]
    fn multi_level_layout_chain_resolves() {
        let base = parse("[@yield('body')]").unwrap();
        let app = parse("@extends('base')@section('body')<@yield('inner')>@endsection").unwrap();
        let page = parse("@extends('app')@section('inner')hi@endsection").unwrap();

        let resolved = resolve(page, &mut |name| match name {
            "app" => Ok(app.clone()),
            "base" => Ok(base.clone()),
            other => panic!("unexpected load({other})"),
        })
        .unwrap();

        let rendered: String = resolved
            .iter()
            .map(|n| match n {
                Node::Text(t) => t.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(rendered, "[<hi>]");
    }

    #[test]
    fn yield_inside_conditional_is_substituted() {
        let layout = parse("@if(show) @yield('content') @endif").unwrap();
        let child = parse("@extends('layout')@section('content')hi@endsection").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        let Node::If { then_branch, .. } = &resolved[0] else {
            panic!("expected If node");
        };
        assert!(then_branch.contains(&Node::Text("hi".to_string())));
    }

    #[test]
    fn yield_inside_spa_is_substituted() {
        // The load-bearing case for `@spa`'s whole design (see `Node::Spa`'s
        // own doc comment in `ast.rs`): its body needs `substitute_yields`
        // to reach the `@yield('content')` sitting inside it exactly as it
        // already does for `@if`'s own `then_branch` above.
        let layout = parse("@spa @yield('content') @endspa").unwrap();
        let child = parse("@extends('layout')@section('content')hi@endsection").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        let Node::Spa(body) = &resolved[0] else {
            panic!("expected Spa node");
        };
        assert!(body.contains(&Node::Text("hi".to_string())));
    }

    #[test]
    fn yield_inside_an_elseif_branch_is_substituted() {
        // `@elseif` desugars into a nested `Node::If` inside the outer
        // `else_branch` (see `larust-view::parser`) — this pins that
        // `substitute_yields`'s generic, unconditional recursion into both
        // `then_branch`/`else_branch` correctly reaches into that nested
        // node too, not just a single top-level `@if`.
        let layout = parse("@if(a) skip @elseif(b) @yield('content') @else skip @endif").unwrap();
        let child = parse("@extends('layout')@section('content')hi@endsection").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        let Node::If {
            else_branch: outer_else,
            ..
        } = &resolved[0]
        else {
            panic!("expected outer If node");
        };
        let Node::If {
            then_branch: nested_then,
            ..
        } = &outer_else[0]
        else {
            panic!("expected nested If node from @elseif");
        };
        assert!(nested_then.contains(&Node::Text("hi".to_string())));
    }

    #[test]
    fn self_extending_template_errors_instead_of_overflowing() {
        let a = parse("@extends('a')").unwrap();

        let result = resolve(a.clone(), &mut |name| {
            assert_eq!(name, "a");
            Ok(a.clone())
        });

        assert!(result.is_err(), "expected a cycle error, got Ok");
    }

    #[test]
    fn mutually_extending_templates_error_instead_of_overflowing() {
        let a = parse("@extends('b')").unwrap();
        let b = parse("@extends('a')").unwrap();

        let result = resolve(a, &mut |name| match name {
            "a" => Ok(a_clone()),
            "b" => Ok(b.clone()),
            other => panic!("unexpected load({other})"),
        });

        assert!(result.is_err(), "expected a cycle error, got Ok");

        fn a_clone() -> Vec<Node> {
            parse("@extends('b')").unwrap()
        }
    }

    #[test]
    fn stack_renders_a_single_push() {
        let layout = parse("<head>@stack('scripts')</head>").unwrap();
        let child = parse("@extends('layout')@push('scripts')<script>a</script>@endpush").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        assert_eq!(
            resolved,
            vec![
                Node::Text("<head>".to_string()),
                Node::Text("<script>a</script>".to_string()),
                Node::Text("</head>".to_string()),
            ]
        );
    }

    #[test]
    fn multiple_pushes_to_the_same_stack_accumulate_in_source_order() {
        // Unlike `@section` (last-write-wins), two `@push`es to the same
        // name both contribute — this is the entire reason `@push`/`@stack`
        // exists instead of just reusing `@section`/`@yield`.
        let layout = parse("@stack('scripts')").unwrap();
        let child = parse(
            "@extends('layout')\
             @push('scripts')one@endpush\
             @push('scripts')two@endpush",
        )
        .unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        assert_eq!(
            resolved,
            vec![Node::Text("one".to_string()), Node::Text("two".to_string())]
        );
    }

    #[test]
    fn pushes_from_every_level_of_a_layout_chain_reach_the_base_stack() {
        // The base layout's `@stack` must see contributions from *both*
        // `app` and `page` — proving pushes are collected across the whole
        // `@extends` chain, not just the single level whose
        // `substitute_yields` call happens to run first.
        let base = parse("[@stack('scripts')]").unwrap();
        let app = parse("@extends('base')@push('scripts')from-app@endpush").unwrap();
        let page = parse("@extends('app')@push('scripts')from-page@endpush").unwrap();

        let resolved = resolve(page, &mut |name| match name {
            "app" => Ok(app.clone()),
            "base" => Ok(base.clone()),
            other => panic!("unexpected load({other})"),
        })
        .unwrap();

        let rendered: String = resolved
            .iter()
            .map(|n| match n {
                Node::Text(t) => t.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(rendered, "[from-pagefrom-app]");
    }

    #[test]
    fn stack_with_no_matching_push_becomes_empty() {
        let layout = parse("<head>@stack('scripts')</head>").unwrap();
        let child = parse("@extends('layout')").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        assert_eq!(
            resolved,
            vec![
                Node::Text("<head>".to_string()),
                Node::Text("</head>".to_string()),
            ]
        );
    }

    #[test]
    fn a_push_inside_a_resource_taged_templates_own_file_reaches_an_outer_stack() {
        // Regression test for the real bug this fix addresses: a `@push`
        // doesn't have to live at a `<resource:...>` tag's *call site*
        // (its `slot`) to be seen — it can live inside the resource's own
        // named template file (real source: `livewire.components.head`,
        // included via `<resource:...>` from every page, wraps its entire
        // body — title, meta description, OG tags — in `@push('head')`).
        // Before this fix, `collect_pushes` only ever recursed into a
        // `Node::Resource`'s `slot`, never `load()`ed and scanned its own
        // named file, so this content was silently dropped everywhere.
        let head = parse("@push('head')<title>hi</title>@endpush").unwrap();
        let page = parse("<head>@stack('head')</head><resource:head></resource:head>").unwrap();

        let resolved = resolve(page, &mut |name| {
            assert_eq!(name, "head");
            Ok(head.clone())
        })
        .unwrap();

        let rendered: String = resolved
            .iter()
            .map(|n| match n {
                Node::Text(t) => t.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(rendered, "<head><title>hi</title></head>");
    }

    #[test]
    fn a_stack_inside_a_resource_tagged_templates_own_file_receives_an_outer_push() {
        // The other half of the same real bug: `@stack('head')` living
        // *inside* the resource file (real source: `components.layouts.
        // app`, included via `<resource:...>` from every page's wire
        // shell) never received pushes from the page that included it,
        // because that resource file is loaded and codegen'd entirely
        // outside `resolve()`'s own traversal. Mirrors exactly what
        // `larust-macros`' `Node::Resource` codegen arm now does: apply
        // `substitute_stacks` to a resource's freshly-loaded nodes using
        // the whole-tree `pushes` map `resolve_with_context` returns.
        let layout = parse("<head>@stack('head')</head>").unwrap();
        let page =
            parse("@push('head')<title>hi</title>@endpush<resource:layout></resource:layout>")
                .unwrap();

        let (_, (pushes, _)) = resolve_with_context(page, &mut |name| {
            assert_eq!(name, "layout");
            Ok(layout.clone())
        })
        .unwrap();

        let resolved_layout = substitute_stacks(layout, &pushes);
        let rendered: String = resolved_layout
            .iter()
            .map(|n| match n {
                Node::Text(t) => t.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(rendered, "<head><title>hi</title></head>");
    }

    #[test]
    fn push_nested_inside_another_push_still_reaches_its_own_stack() {
        // Regression test: `collect_pushes` originally only registered a
        // `@push`'s own body under its own name — it never recursed *into*
        // that body looking for a further-nested `@push`, so this exact
        // case silently dropped "nested" entirely (registered nowhere).
        let layout = parse("[@stack('outer')][@stack('inner')]").unwrap();
        let child =
            parse("@extends('layout')@push('outer')@push('inner')nested@endpush@endpush").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        let rendered: String = resolved
            .iter()
            .map(|n| match n {
                Node::Text(t) => t.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(rendered, "[][nested]");
    }

    #[test]
    fn push_inside_foreach_is_rejected_with_a_clear_error() {
        // Regression test: `@push`/`@stack` resolve once, statically, at
        // compile time — there is no per-iteration runtime step the way
        // Laravel's own imperative Blade compiler has, so a push inside a
        // loop must be rejected rather than silently rendering once
        // instead of once-per-item (or, worse, referencing the loop
        // variable and failing to compile with a confusing error far from
        // the actual template source).
        let layout = parse("@stack('scripts')").unwrap();
        let child = parse(
            "@extends('layout')\
             @foreach(item in items)@push('scripts'){{ item }}@endpush@endforeach",
        )
        .unwrap();

        let err = resolve(child, &mut |_| Ok(layout.clone())).unwrap_err();
        assert!(err.to_string().contains("@push"));
        assert!(err.to_string().contains("@foreach"));
    }

    #[test]
    fn push_inside_a_nested_foreach_is_also_rejected() {
        let layout = parse("@stack('scripts')").unwrap();
        let child = parse(
            "@extends('layout')\
             @foreach(group in groups)@foreach(item in group.items)\
             @push('scripts'){{ item }}@endpush\
             @endforeach@endforeach",
        )
        .unwrap();

        assert!(resolve(child, &mut |_| Ok(layout.clone())).is_err());
    }

    #[test]
    fn global_reaches_through_an_indifferent_middle_layout() {
        // The exact bug pattern `@section`/`@yield`'s eager per-level
        // resolution has and `@push`/`@stack`'s two-pass design avoids:
        // `app` (the middle layout) doesn't set `@globals` at all, so a
        // per-level-eager resolver would blank `base`'s `@global(title)`
        // before `page`'s `@globals` ever got a chance. The whole-chain
        // collect-then-substitute design here must not have that gap.
        let base = parse("<title>@global(title)</title>").unwrap();
        let app = parse("@extends('base')").unwrap();
        let page = parse("@extends('app')@globals\ntitle = \"Hi\"\n@endglobals").unwrap();

        let resolved = resolve(page, &mut |name| match name {
            "app" => Ok(app.clone()),
            "base" => Ok(base.clone()),
            other => panic!("unexpected load({other})"),
        })
        .unwrap();

        assert_eq!(
            resolved,
            vec![
                Node::Text("<title>".to_string()),
                Node::Interpolate {
                    expr: "\"Hi\"".to_string(),
                    escape: true,
                },
                Node::Text("</title>".to_string()),
            ]
        );
    }

    #[test]
    fn globals_from_child_page_override_a_middle_layout_that_also_sets_it() {
        let base = parse("<title>@global(title)</title>").unwrap();
        let app = parse("@extends('base')@globals\ntitle = \"App default\"\n@endglobals").unwrap();
        let page =
            parse("@extends('app')@globals\ntitle = \"Page specific\"\n@endglobals").unwrap();

        let resolved = resolve(page, &mut |name| match name {
            "app" => Ok(app.clone()),
            "base" => Ok(base.clone()),
            other => panic!("unexpected load({other})"),
        })
        .unwrap();

        assert_eq!(
            resolved,
            vec![
                Node::Text("<title>".to_string()),
                Node::Interpolate {
                    expr: "\"Page specific\"".to_string(),
                    escape: true,
                },
                Node::Text("</title>".to_string()),
            ]
        );
    }

    #[test]
    fn global_with_no_matching_globals_becomes_empty() {
        let layout = parse("<title>@global(title)</title>").unwrap();
        let child = parse("@extends('layout')").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        assert_eq!(
            resolved,
            vec![
                Node::Text("<title>".to_string()),
                Node::Text("</title>".to_string()),
            ]
        );
    }

    #[test]
    fn global_falls_back_to_its_default_when_nothing_sets_it() {
        let layout = parse(r#"<title>@global(title, "Larust")</title>"#).unwrap();
        let child = parse("@extends('layout')").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        assert_eq!(
            resolved,
            vec![
                Node::Text("<title>".to_string()),
                Node::Interpolate {
                    expr: "\"Larust\"".to_string(),
                    escape: true,
                },
                Node::Text("</title>".to_string()),
            ]
        );
    }

    #[test]
    fn a_matching_globals_entry_wins_over_the_fallback() {
        let layout = parse(r#"<title>@global(title, "Larust")</title>"#).unwrap();
        let child = parse("@extends('layout')@globals\ntitle = \"Page\"\n@endglobals").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        assert_eq!(
            resolved,
            vec![
                Node::Text("<title>".to_string()),
                Node::Interpolate {
                    expr: "\"Page\"".to_string(),
                    escape: true,
                },
                Node::Text("</title>".to_string()),
            ]
        );
    }

    #[test]
    fn multiple_globals_blocks_accumulate_different_names() {
        let layout = parse("[@global(a)][@global(b)]").unwrap();
        let child = parse(
            "@extends('layout')\
             @globals\na = \"1\"\n@endglobals\
             @globals\nb = \"2\"\n@endglobals",
        )
        .unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        let rendered: String = resolved
            .iter()
            .map(|n| match n {
                Node::Text(t) => t.as_str(),
                Node::Interpolate { expr, .. } => expr.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(rendered, "[\"1\"][\"2\"]");
    }

    #[test]
    fn globals_inside_foreach_is_rejected_with_a_clear_error() {
        let layout = parse("@global(title)").unwrap();
        let child = parse(
            "@extends('layout')\
             @foreach(item in items)@globals\ntitle = item\n@endglobals@endforeach",
        )
        .unwrap();

        let err = resolve(child, &mut |_| Ok(layout.clone())).unwrap_err();
        assert!(err.to_string().contains("@globals"));
        assert!(err.to_string().contains("@foreach"));
    }

    #[test]
    fn globals_inside_if_is_rejected_with_a_clear_error() {
        // Regression test: a compile-time collection pass can't express
        // "pick whichever branch the condition selects at runtime" — before
        // this rejection existed, whichever branch was walked *last*
        // (`else_branch`) always silently won, regardless of the `@if`
        // condition's actual runtime value.
        let layout = parse("@global(title)").unwrap();
        let child = parse(
            "@extends('layout')\
             @if(is_admin)@globals\ntitle = \"Admin\"\n@endglobals\
             @else@globals\ntitle = \"User\"\n@endglobals@endif",
        )
        .unwrap();

        let err = resolve(child, &mut |_| Ok(layout.clone())).unwrap_err();
        assert!(err.to_string().contains("@globals"));
        assert!(err.to_string().contains("@if"));
    }

    #[test]
    fn globals_inside_can_is_rejected_with_a_clear_error() {
        let layout = parse("@global(title)").unwrap();
        let child = parse(
            "@extends('layout')\
             @can(Permission::EditPosts)@globals\ntitle = \"Editor\"\n@endglobals@endcan",
        )
        .unwrap();

        let err = resolve(child, &mut |_| Ok(layout.clone())).unwrap_err();
        assert!(err.to_string().contains("@globals"));
        assert!(err.to_string().contains("@can"));
    }

    #[test]
    fn globals_inside_role_is_rejected_with_a_clear_error() {
        let layout = parse("@global(title)").unwrap();
        let child = parse(
            "@extends('layout')\
             @role(Role::Admin)@globals\ntitle = \"Admin\"\n@endglobals@endrole",
        )
        .unwrap();

        let err = resolve(child, &mut |_| Ok(layout.clone())).unwrap_err();
        assert!(err.to_string().contains("@globals"));
        assert!(err.to_string().contains("@role"));
    }

    #[test]
    fn global_placeholder_inside_can_is_still_substituted() {
        // Unlike a `@globals` *block* (rejected above), a `@global(...)`
        // *read* inside `@can`/`@role` is fine — only compile-time
        // collection (deciding a value) is the problem, not using an
        // already-resolved value inside a conditionally-rendered branch.
        let layout = parse("@can(Permission::EditPosts)@global(title)@endcan").unwrap();
        let child = parse("@extends('layout')@globals\ntitle = \"Hi\"\n@endglobals").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        let Node::Can { then_branch, .. } = &resolved[0] else {
            panic!("expected Can node");
        };
        assert_eq!(
            then_branch[0],
            Node::Interpolate {
                expr: "\"Hi\"".to_string(),
                escape: true,
            }
        );
    }

    #[test]
    fn globals_inside_elseif_is_also_rejected() {
        // `@elseif` desugars into a nested `Node::If` in the outer
        // `else_branch` — confirms the rejection reaches that nested case
        // too, not just a single top-level `@if`.
        let layout = parse("@global(title)").unwrap();
        let child = parse(
            "@extends('layout')\
             @if(a) x @elseif(b)@globals\ntitle = \"B\"\n@endglobals@endif",
        )
        .unwrap();

        assert!(resolve(child, &mut |_| Ok(layout.clone())).is_err());
    }

    #[test]
    fn global_works_without_any_extends_at_all() {
        // Unlike `@section`/`@yield` (which never resolve at all without an
        // `@extends`), `@global`/`@globals` follow `@push`/`@stack`'s
        // shape: `collect_globals` runs unconditionally at the top of
        // `resolve_inner`, before the `@extends` check, so a single
        // standalone template can set and read its own global.
        let nodes =
            parse("@globals\ntitle = \"Solo\"\n@endglobals<title>@global(title)</title>").unwrap();

        let resolved = resolve(nodes, &mut |_| unreachable!()).unwrap();

        // The leftover `Node::Globals` node itself (never consumed by
        // `resolve()` — same as an unresolved `Node::Push` — since there's
        // no `@extends` chain to consume its metadata into) renders as
        // nothing via codegen's fallback, same story as an unresolved
        // `Push`; a rendered-string comparison (rather than exact `Vec<Node>`
        // equality) reflects that without needing to spell out that leftover
        // node here.
        let rendered: String = resolved
            .iter()
            .map(|n| match n {
                Node::Text(t) => t.as_str(),
                Node::Interpolate { expr, .. } => expr.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(rendered, "<title>\"Solo\"</title>");
    }

    #[test]
    fn persist_global_substitutes_to_a_persist_global_node_not_interpolate() {
        let layout = parse(r#"<html data-theme="@global(theme, "dark")">"#).unwrap();
        let child =
            parse("@extends('layout')@globals\npersist theme = \"light\"\n@endglobals").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        assert_eq!(
            resolved,
            vec![
                Node::Text("<html data-theme=\"".to_string()),
                Node::PersistGlobal {
                    cookie_name: "theme".to_string(),
                    fallback_expr: "\"light\"".to_string(),
                },
                Node::Text("\">".to_string()),
            ]
        );
    }

    #[test]
    fn a_non_persist_global_still_substitutes_to_interpolate() {
        let layout = parse("<title>@global(title)</title>").unwrap();
        let child = parse("@extends('layout')@globals\ntitle = \"Hi\"\n@endglobals").unwrap();

        let resolved = resolve(child, &mut |_| Ok(layout.clone())).unwrap();

        assert_eq!(
            resolved,
            vec![
                Node::Text("<title>".to_string()),
                Node::Interpolate {
                    expr: "\"Hi\"".to_string(),
                    escape: true,
                },
                Node::Text("</title>".to_string()),
            ]
        );
    }

    #[test]
    fn a_child_pages_persist_entry_still_overrides_a_middle_layout_that_also_sets_it() {
        // Same "child wins, indifferent middle layout doesn't block it"
        // shape as `globals_from_child_page_override_a_middle_layout_that_also_sets_it`
        // above, just for a `persist` entry — proving `GlobalEntry`'s
        // `persist` flag travels correctly through the same child-wins
        // merge, not just the plain-expression case.
        let base = parse("<title>@global(theme, \"dark\")</title>").unwrap();
        let app = parse("@extends('base')@globals\npersist theme = \"app-default\"\n@endglobals")
            .unwrap();
        let page = parse("@extends('app')@globals\npersist theme = \"page-specific\"\n@endglobals")
            .unwrap();

        let resolved = resolve(page, &mut |name| match name {
            "app" => Ok(app.clone()),
            "base" => Ok(base.clone()),
            other => panic!("unexpected load({other})"),
        })
        .unwrap();

        assert_eq!(
            resolved,
            vec![
                Node::Text("<title>".to_string()),
                Node::PersistGlobal {
                    cookie_name: "theme".to_string(),
                    fallback_expr: "\"page-specific\"".to_string(),
                },
                Node::Text("</title>".to_string()),
            ]
        );
    }
}
