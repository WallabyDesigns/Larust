---
title: Reactive Components
parent: Digging Deeper
nav_order: 5
---

# Reactive Components
{: .no_toc }

`@wire(...)` components are Larust's Livewire-equivalent: interactive,
server-driven UI - live search, inline validation, click-to-update -
with no separate frontend framework, no build step, and no client-side
state management of your own to write.

1. TOC
{:toc}

## The key difference from Livewire

Livewire signs and ships a component's state to the browser and back on
every interaction. Larust's `@wire` components **never leave the
server**: state lives in the session, keyed by an opaque component id;
the browser only ever holds that id and whatever HTML was last rendered.
Smaller wire payloads, and nothing about your component's internal state
shape is ever exposed to the client.

## `WireComponent`

```rust
use larust_live::WireComponent;

#[derive(Serialize, Deserialize)]
pub struct Counter {
    count: i64,
}

impl WireComponent for Counter {
    const NAME: &'static str = "counter";

    async fn mount(_session: &Session, props: &HashMap<String, Value>) -> Self {
        let count = props.get("start").and_then(Value::as_i64).unwrap_or(0);
        Counter { count }
    }

    async fn render(&self) -> View {
        larust_support::view!("components.counter", { count: self.count })
    }

    async fn call(&mut self, _session: &Session, action: &str, _args: &Value) -> Result<Option<String>, AppError> {
        match action {
            "increment" => { self.count += 1; Ok(None) }
            "decrement" => { self.count -= 1; Ok(None) }
            _ => Err(AppError::Http { status: StatusCode::BAD_REQUEST, message: format!("unknown action: {action}") }),
        }
    }
}
```

- **`mount`** builds initial state from whatever props the `@wire(...)`
  call site passed - a prop this component doesn't recognize is simply
  ignored. It receives the real session too, so a component can capture
  per-viewer identity once (does the current user own this record, so
  should edit/delete controls even render) rather than being told again
  on every later action.
- **`render`** returns a `View` - normally a `view!(...)` call, same as a
  full page. Use `.into_html()` when you need the raw string (a fragment
  must never get the dev-reload script a full page's `into_response()`
  would splice in).
- **`call`** dispatches a named action, mutating `self` in place. The
  default implementation rejects every action name outright - a
  display-only, `wire:model`-only component (no clicks or submits at all)
  needs zero action boilerplate, and there's no risk of an
  accidentally-permissive default the way there would be if unrecognized
  actions were silently allowed.

Register it once, at boot, alongside your routes:

```rust
larust_support::wire::components()
    .register::<Counter>()
    .publish();
```

## Mounting one in a template

```
<wire:counter start="10" />
```
or the directive spelling:
```
@wire('counter', { start: 10 })
```

Both parse to the same thing; the tag form reads better for a component
whose props are mostly simple/literal.

## The client directives

| Directive | Behavior |
|---|---|
| `wire:model="field"` | Syncs an input's value to `field`, deferred (sent on the next action, not every keystroke) |
| `wire:model.live="field"` | Same, but sends on every input event instead of waiting |
| `wire:click="action_name"` | Calls `action_name` on click |
| `wire:submit="action_name"` | Calls `action_name` on form submit (preventing the native submit) |
| `wire:ignore` | Opts an element out of the client's DOM patching - for anything with its own state a re-render would clobber (a rich-text editor, say) |

{% raw %}```
<!-- resources/views/components/counter.blade.xr -->
<div>
    <span>{{ count }}</span>
    <button wire:click="increment">+</button>
    <button wire:click="decrement">-</button>
</div>
```{% endraw %}

Every interaction posts to `/__larust_wire/{component_id}` (registered
automatically via `larust_support::wire::WirePlugin` - see
[Routing](../../the-basics/routing#plugins)), re-renders the component
server-side, and patches the returned fragment into the page via a small
vendored client runtime - no bundler, no npm dependency, injected
automatically by `@larustscripts` in your layout.

## `@loadonce`

```
@loadonce
    <link rel="stylesheet" href="/styles/rich-editor.css">
    <script src="/scripts/rich-editor.js"></script>
@endloadonce
```

Compile-time sugar over `wire:ignore` for colocating a component's own
static assets so a live re-render doesn't re-fetch or re-initialize them.

## Actions that redirect

```rust
async fn call(&mut self, session: &Session, action: &str, _args: &Value) -> Result<Option<String>, AppError> {
    if action == "submit" {
        let post = Post::create(NewPost { /* ... */ }).await?;
        return Ok(Some(format!("/posts/{}", post.id)));  // navigate the browser, not just re-render
    }
    Ok(None)
}
```

`Ok(Some(path))` tells the client to navigate the browser to `path`
instead of patching the current fragment in place - Livewire's own
`redirect()`, for the common case of a `wire:submit` that finishes by
sending the user somewhere else entirely.

## Validation inside a component

A `wire:submit` form typically re-runs validation on every submit and
stores any errors as component state for `render()` to display -
`app/Wire/post_form.rs` in the reference app is the real, complete
working example of this: reactive per-field validation errors, a
Trix rich-text editor kept out of the patcher's way via `wire:ignore`,
and an `Ok(Some(path))` redirect on success. `app/Wire/post_list.rs`
alongside it is a live-filtered list (`wire:model.live` driving a search
box) with per-viewer edit/delete controls computed once in `mount`. Both
are worth reading end to end once the shape above makes sense.

## Next

[Realtime & Broadcasting](../../digging-deeper/realtime-and-broadcasting) covers `@wire`'s
counterpart for genuinely *server-pushed* updates - a change made by one
user, shown live to everyone else already looking at the page.
