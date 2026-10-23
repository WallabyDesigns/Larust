---
title: Realtime & Broadcasting
parent: Digging Deeper
nav_order: 6
---

# Realtime & Broadcasting
{: .no_toc }

Three distinct mechanisms, each solving a different real-time shape.
[Reactive Components](../../digging-deeper/reactive-components) (`@wire`) handle *this
viewer's own* interactions; everything on this page is about pushing
updates *to* a viewer from something that happened elsewhere.

1. TOC
{:toc}

## `@live(...)` - server-pushed HTML fragments

For "re-render this exact block for everyone currently looking at it" -
a live comment count, a ticker, anything that's naturally just HTML:

{% raw %}```
<div>
    @live("posts.count")
        You have {{ post_count }} posts.
    @endlive
</div>
```{% endraw %}

The body renders once, inline, wrapped in `<div
data-live-channel="posts.count">`. Whenever something relevant happens
elsewhere in the app:

```rust
let fresh_html = larust_support::view!("components.post-count-fragment", { post_count });
larust_support::push::broadcast("posts.count", fresh_html.into_html());
```

Every subscribed tab receives the new fragment over a WebSocket
(`GET /__larust_push/{channel}`, registered via `.plugin(PushPlugin)`)
and a small vendored client runtime patches it into the matching
`[data-live-channel]` element - a harmless no-op if nobody's listening. A
channel is just a string key, created lazily on first use - no per-channel
registration step, no component trait.

`push::wrap(channel, inner_html)` produces the exact same wrapper markup
`@live` itself renders, so a broadcast payload can never structurally
drift from what the initial render already put on the page - build a
broadcast's HTML from the same template the `@live` block itself uses,
wrapped once with this, rather than reconstructing the wrapper by hand.

For anything that isn't "swap this HTML fragment" - a typing indicator,
appending one new comment via your own JS rather than re-rendering a
whole list, any event your client-side code wants to react to directly -
see [Reverb](../../optional-features/reverb), an optional generic WebSocket
pub/sub server. It's deliberately a separate route namespace and channel
registry from `@live` above, on purpose - pointing both at the same
channel name would mix incompatible payload shapes (raw HTML vs. a
`{event, data}` envelope).

## SPA-style navigation: `@spa`

Turbo/Livewire-SPA-style page transitions - intercepts same-origin link
clicks and form submissions, fetches the destination normally (the exact
same full HTML `view!(...)` already renders for a hard reload - there is
no separate server-side rendering path for this), and swaps in what
changed via the History API instead of a full reload:

```
<!-- layouts/app.blade.xr -->
@spa
    <header>...</header>
    <main>@yield('content')</main>
@endspa
```

```rust
route.plugin(larust_support::spa::SpaPlugin)
```

A `#[derive(FormRequest)]` validation failure (422) is handled specially
rather than triggering a pointless native resubmit: the client dispatches
a `larust:spa:validation-error` event (`{ url, message, errors }`) for
your own JS to render inline, since a 422 always runs before the handler
and can never represent a partial mutation - provably safe to skip a
resubmit for this one status specifically.

```js
document.addEventListener("larust:spa:validation-error", (e) => {
    renderInlineErrors(e.detail.errors);
});
```

## Next

[Plugins](../../digging-deeper/plugins).
