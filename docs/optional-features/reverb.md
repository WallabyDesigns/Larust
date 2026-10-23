---
title: Reverb
parent: Optional Features
nav_order: 7
---

# Reverb
{: .no_toc }

A generic WebSocket pub/sub server - Larust's port of
[Laravel Reverb](https://laravel.com/docs/reverb). For anything that
isn't "swap this HTML fragment" - a typing indicator, appending one new
comment via your own JS rather than re-rendering a whole list, any event
your client-side code wants to react to directly. For HTML-fragment
swaps instead, see `@live(...)` in
[Realtime & Broadcasting](../../digging-deeper/realtime-and-broadcasting).

1. TOC
{:toc}

## Adding this feature

```bash
# At scaffold time
xr new myapp --features reverb

# Or retrofit onto an existing app
xr add reverb
```

Both are equivalent to adding `"reverb"` to `larust-support`'s
`features = [...]` list in your app's own `Cargo.toml` by hand.

## Broadcasting an event

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

## Private channels

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

## `@live` vs. `larust-reverb` - which one?

| | `@live` (`larust-live::push`) | `larust-reverb` |
|---|---|---|
| Payload | Pre-rendered HTML, replaces a fixed DOM element | Arbitrary JSON, tagged with an event name |
| Client handling | Automatic (the vendored patcher) | Your own JS `.listen(...)` callback |
| Best for | A block that's naturally "just render this again" | Anything needing custom client-side behavior on arrival |
| Private channels | Not yet | Yes (`private-*` + `authorize(...)`) |

They're deliberately separate route namespaces and separate channel
registries, on purpose - pointing both at the same channel name would mix
incompatible payload shapes (raw HTML vs. a `{event, data}` envelope).
See [Realtime & Broadcasting](../../digging-deeper/realtime-and-broadcasting)
for `@live` itself and SPA-style navigation.

## Next

Back to [Optional Features](../../optional-features/) for the full
list, or [Realtime & Broadcasting](../../digging-deeper/realtime-and-broadcasting)
for `@live(...)` and `@spa`.
