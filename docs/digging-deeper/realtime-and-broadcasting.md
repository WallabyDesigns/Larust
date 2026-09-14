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

## `larust-reverb` - a generic WebSocket pub/sub server

For anything that isn't "swap this HTML fragment" - a typing indicator,
appending one new comment via your own JS rather than re-rendering a
whole list, any event your client-side code wants to react to directly.
Larust's port of [Laravel Reverb](https://laravel.com/docs/reverb):

```rust
larust_support::reverb::broadcast_event(
    &format!("post.{}", post_id),
    "CommentAdded",
    &comment,   // any Serialize payload
)?;
```

```js
LarustReverb.channel(`post.${postId}`).listen("CommentAdded", (comment) => {
    appendCommentToDom(comment);
});
```

An app that uses it registers the runtime script + WebSocket route once:

```rust
route.plugin(larust_support::reverb::ReverbPlugin)
```

### Private channels

A channel name starting with `private-` requires authorization -
register one callback, checked at WebSocket-upgrade time (the browser
already carries the session cookie to this same-origin connection, so
there's no separate Pusher-style `/broadcasting/auth` round trip to
implement):

```rust
larust_support::reverb::authorize(|session, channel| async move {
    let Ok(Some(user)) = larust_support::auth::user::<User>(&session).await else {
        return false;
    };
    channel == format!("private-orders.{}", user.id)
});
```

### `@live` vs. `larust-reverb` - which one?

| | `@live` (`larust-live::push`) | `larust-reverb` |
|---|---|---|
| Payload | Pre-rendered HTML, replaces a fixed DOM element | Arbitrary JSON, tagged with an event name |
| Client handling | Automatic (the vendored patcher) | Your own JS `.listen(...)` callback |
| Best for | A block that's naturally "just render this again" | Anything needing custom client-side behavior on arrival |
| Private channels | Not yet | Yes (`private-*` + `authorize(...)`) |

They're deliberately separate route namespaces and separate channel
registries, on purpose - pointing both at the same channel name would mix
incompatible payload shapes (raw HTML vs. a `{event, data}` envelope).

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

[Social Login & Plugins](../../digging-deeper/social-login-and-plugins).
