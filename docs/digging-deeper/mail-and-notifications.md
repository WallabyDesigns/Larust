---
title: Mail & Notifications
parent: Digging Deeper
nav_order: 2
---

# Mail & Notifications
{: .no_toc }

1. TOC
{:toc}

## Mail

A `Mailable` is a plain struct implementing two methods:

```rust
pub struct WelcomeMail<'a> {
    pub user: &'a User,
}

impl Mailable for WelcomeMail<'_> {
    fn subject(&self) -> String {
        format!("Welcome, {}!", self.user.name)
    }

    fn html_body(&self) -> String {
        format!("<h1>Welcome, {}</h1><p>Glad to have you.</p>", self.user.name)
    }
}
```

```rust
mail().to(&user.email).send(WelcomeMail { user: &user }).await?;
```

`Mailable` deliberately has no `Serialize`/`'static` bound - unlike a
queued `Job`, a mailable is free to borrow (`&'a User`, as above) since
`.send()` renders it immediately, synchronously, at the call site.

```
# .env
MAIL_DRIVER=log    # writes the rendered subject/body to your app's log -
                    # no SMTP server needed for local dev or `cargo test`
# MAIL_DRIVER=smtp # fill in MAIL_HOST/PORT/USERNAME/PASSWORD/ENCRYPTION to send for real
```

### Queued mail

```rust
mail().to(&user.email).queue(WelcomeMail { user: &user }).await?;
```

Because a mailable can borrow and isn't required to be `Serialize`,
`.queue()` can't hand the *mailable itself* off to a worker the way an
app-defined `Job` does. Instead it renders `subject()`/`html_body()`
**immediately**, synchronously, at the call site - the same rendering
`.send()` does - and enqueues only the already-rendered
`{to, subject, html_body}` for a background worker to actually deliver.
This is a deliberate, narrower deferral than Laravel's own `.queue()`
(which re-resolves and re-renders your mailable fresh on the worker): it
defers the SMTP/network I/O, not the rendering.

A freshly scaffolded app registers this framework's own `MailJob` in its
`queue:work` dispatch by default (see [Events, Queues, Scheduling &
Commands](../../digging-deeper/events-queues-and-scheduling#registering-job-types)) so
`.queue()` works with zero setup - an idle registration costs nothing if
you never call it.

### Testing mail

```rust
larust_testing::fake();

// ... exercise a route that sends mail ...

larust_testing::assert_sent::<WelcomeMail>(|sent| sent.to.contains(&user.email));
larust_testing::assert_not_sent::<PostPublishedMail>(|_| true);
```

`fake()` records every `.send()`/`.queue()` call instead of actually
dispatching it (log or smtp alike) - reached only through
`larust-testing`, never the production `larust_support::mail` facade, so
there's no risk of accidentally leaving a test-only mode reachable from
real app code. A faked `.queue()` call is recorded identically to
`.send()` - there's no separate "assert queued vs. assert sent"
distinction yet.

## Notifications

Laravel's *database* notification channel specifically - not a
multi-channel dispatch table. A `Notification` is a plain, serializable
struct with one constant:

```rust
#[derive(Serialize)]
pub struct PostPublished {
    pub post_id: i64,
    pub title: String,
}

impl Notification for PostPublished {
    const NOTIFICATION_TYPE: &'static str = "post_published";
}
```

```rust
notification::notify(&author, &PostPublished { post_id, title }).await?;

let recent = notification::notifications_for(&user, 20).await?;  // Vec<StoredNotification>, newest first
let unread = notification::unread_count(&user).await?;
notification::mark_as_read(&user, notification_id).await?;       // 403s if it isn't this user's own
notification::mark_all_as_read(&user).await?;
```

`StoredNotification::data` stays a raw `serde_json::Value` rather than a
concrete type, since one query can read rows across many different
`Notification` types at once - match on `notification_type` to interpret
`data` if a generic display (title/timestamp/read state) isn't enough.

### Why no `via()`/multi-channel dispatch

Laravel's `Notification::via()` decides at runtime which channels
(`mail`, `database`, `broadcast`, ...) a notification goes out on. This
framework's equivalent traits (`Mailable`, `Job`, `Authenticatable`) are
all zero-default-method traits by convention - building a `via()`-style
dispatcher here would be the first thing in this codebase to break that
"a real gap is a compile error, not a silently-skipped optional method"
rule. Since `Mail` and the WebSocket `push::broadcast` (see [Realtime &
Broadcasting](../../digging-deeper/realtime-and-broadcasting)) already each fully solve their
own job independently, reaching more than one channel is just composing
ordinary, independently-typed calls at the same call site - exactly what
the real listener example below does. A convenience for the common case
of exactly two channels at once exists too:

```rust
notification::notify_and_mail(&author, &PostPublished { post_id, title }, WelcomeMail { user: &author }).await?;
```

Runs both concurrently via `tokio::try_join!` rather than one after the
other.

## A real example, composed

This is the reference app's actual `PostCreated` listener - one event,
three independently-composed reactions, no framework dispatch table
tying them together:

```rust
event::listeners().on::<PostCreated, _, _>(|event: PostCreated| async move {
    // 1. Queued - a background job, decoupled from the request.
    queue::dispatch(&NotifyPostCreatedJob { post_id: event.post_id }).await?;

    let author = User::find(event.user_id).await?.unwrap();

    // 2. Database notification - shows up in the author's own notification list.
    notification::notify(&author, &PostPublished {
        post_id: event.post_id, title: event.title.clone(),
    }).await?;

    // 3. Mail - a real email, sent (or logged) immediately.
    mail().to(&author.email).send(PostPublishedMail {
        author: &author, post_title: &event.title, post_id: event.post_id,
    }).await?;
});
```

See [Events, Queues, Scheduling & Commands](../../digging-deeper/events-queues-and-scheduling) for
`event::dispatch`/`listeners()` and `Job`/`queue::dispatch` themselves.

## Next

[Events, Queues, Scheduling & Commands](../../digging-deeper/events-queues-and-scheduling).
