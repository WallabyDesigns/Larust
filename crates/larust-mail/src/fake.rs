use crate::Mailable;
use std::sync::{Mutex, OnceLock};

static FAKE_SENT: OnceLock<Mutex<Vec<SentMail>>> = OnceLock::new();

/// Activates recording — every `send()` call for the rest of this
/// process records instead of dispatching (log/smtp), regardless of
/// `mail_driver`. Idempotent: a second call in the same process is a
/// no-op (first-writer-wins, matching every other process-wide registry
/// in this codebase — `larust_orm::connect()`, the route-name registry,
/// the event-listener registry). The recorded list from the first
/// activation keeps accumulating; nothing resets it.
pub fn fake() {
    FAKE_SENT.get_or_init(|| Mutex::new(Vec::new()));
}

/// Whether `fake()` has been called — `MailBuilder::send` checks this
/// before building a `SentMail` at all, so the common, real-dispatch case
/// (`fake()` never called) never pays for cloning the rendered body/
/// recipients just to immediately drop them.
pub(crate) fn is_active() -> bool {
    FAKE_SENT.get().is_some()
}

/// A rendered, already-sent (recorded, not dispatched) email — the
/// *output* of a `Mailable`, not the typed instance itself. Recording the
/// instance would require `Mailable: 'static`, which would force every
/// Mailable (including the real `WelcomeMail<'a>`, which borrows its
/// `User`) to own its data instead of borrowing — a breaking constraint
/// this design avoids entirely.
#[derive(Debug, Clone)]
pub struct SentMail {
    pub(crate) mailable_type: &'static str,
    pub to: Vec<String>,
    pub subject: String,
    pub html_body: String,
}

/// Records `mail` if `fake()` has been called; returns whether it did.
/// `MailBuilder::send` uses the return value to decide whether to fall
/// through to the real log/smtp dispatch — `false` (the common case,
/// `fake()` never called) means recording didn't happen and nothing else
/// about `send()`'s behavior changes.
pub(crate) fn record(mail: SentMail) -> bool {
    let Some(recorder) = FAKE_SENT.get() else {
        return false;
    };
    recorder.lock().unwrap().push(mail);
    true
}

/// Panics unless at least one recorded `M` satisfies `predicate`.
///
/// Computes its result and releases the `Mutex` guard *before* asserting
/// — panicking with a lock still held would poison it (`std::sync::Mutex`
/// poisons on an unwind through a held lock), breaking every later
/// `assert_sent`/`assert_not_sent`/`send()` call in the same process with
/// a confusing `PoisonError` instead of the real assertion failure.
pub fn assert_sent<M: Mailable>(predicate: impl Fn(&SentMail) -> bool) {
    let type_name = std::any::type_name::<M>();
    let matching: Vec<SentMail> = {
        let sent = FAKE_SENT
            .get()
            .expect("Mail::fake() was not called before assert_sent()")
            .lock()
            .unwrap();
        sent.iter()
            .filter(|mail| mail.mailable_type == type_name)
            .cloned()
            .collect()
    };
    assert!(
        matching.iter().any(predicate),
        "expected a `{type_name}` to have been sent matching the predicate, \
         but none did (sent of this type: {matching:#?})"
    );
}

/// Panics if any recorded `M` satisfies `predicate`. Same lock-then-drop-
/// before-asserting shape as `assert_sent`, for the same reason.
pub fn assert_not_sent<M: Mailable>(predicate: impl Fn(&SentMail) -> bool) {
    let type_name = std::any::type_name::<M>();
    let matched: bool = {
        let sent = FAKE_SENT
            .get()
            .expect("Mail::fake() was not called before assert_not_sent()")
            .lock()
            .unwrap();
        sent.iter()
            .any(|mail| mail.mailable_type == type_name && predicate(mail))
    };
    assert!(
        !matched,
        "expected no `{type_name}` to have been sent matching the predicate, but one did"
    );
}

#[cfg(test)]
mod tests {
    use crate::mail;

    // All scenarios live in one `#[tokio::test]` fn, not several — every
    // scenario shares the one process-wide `FAKE_SENT` list (it's never
    // reset, by design; see `fake()`'s own doc comment), and separate
    // `#[test]`/`#[tokio::test]` fns in one binary aren't guaranteed to
    // run in any particular order or without overlapping (same
    // constraint `larust-testing/tests/db_test.rs` documents). Distinct
    // dummy `Mailable` types per scenario keep them isolated from each
    // other via `assert_sent`/`assert_not_sent`'s own type-based
    // filtering, the same way `larust-events`' `dispatch_test.rs` used
    // distinct event types to prove type isolation.

    struct Greeting;
    impl crate::Mailable for Greeting {
        fn subject(&self) -> String {
            "Hello".to_string()
        }
        fn html_body(&self) -> String {
            "<p>Hi</p>".to_string()
        }
    }

    struct Farewell;
    impl crate::Mailable for Farewell {
        fn subject(&self) -> String {
            "Goodbye".to_string()
        }
        fn html_body(&self) -> String {
            "<p>Bye</p>".to_string()
        }
    }

    struct Reminder;
    impl crate::Mailable for Reminder {
        fn subject(&self) -> String {
            "Don't forget".to_string()
        }
        fn html_body(&self) -> String {
            "<p>Reminder</p>".to_string()
        }
    }

    #[tokio::test]
    async fn fake_mode_records_instead_of_dispatching_and_assertions_work() {
        // Calling `fake()` and `send()` here, with `larust_core::config()`
        // never having been populated by an `Application::new()` call
        // anywhere in this test binary, and this *not* panicking, is
        // itself the proof that `send()`'s real log/smtp dispatch (which
        // reads `config()` and would panic without it) was never reached
        // — the fake short-circuit happens first.
        super::fake();

        mail().to("alice@example.com").send(Greeting).await.unwrap();

        // A matching recorded `Greeting` is found.
        super::assert_sent::<Greeting>(|sent| sent.to == vec!["alice@example.com".to_string()]);

        // A `Greeting` was sent, but not to this address.
        let result = std::panic::catch_unwind(|| {
            super::assert_sent::<Greeting>(|sent| sent.to == vec!["bob@example.com".to_string()]);
        });
        assert!(
            result.is_err(),
            "assert_sent should panic when no recorded mail matches the predicate"
        );

        // No `Farewell` was ever sent — type isolation from `Greeting`.
        let result = std::panic::catch_unwind(|| {
            super::assert_sent::<Farewell>(|_| true);
        });
        assert!(
            result.is_err(),
            "assert_sent should panic for a type nothing was ever sent as"
        );
        super::assert_not_sent::<Farewell>(|_| true);

        // `assert_not_sent` for `Greeting` itself panics, since one *was*
        // sent matching this always-true predicate.
        let result = std::panic::catch_unwind(|| {
            super::assert_not_sent::<Greeting>(|_| true);
        });
        assert!(
            result.is_err(),
            "assert_not_sent should panic when a matching mail was in fact sent"
        );

        // `.queue()` folds into the exact same recorded list as `.send()`
        // under fake mode — there's no separate `assertQueued` concept
        // yet (see `docs/ARCHITECTURE.md`'s Mail section). Reaching this
        // without a `larust_queue::dispatch()` call ever touching a real
        // (nonexistent, in this test) database is itself the proof that
        // `.queue()`'s fake short-circuit, like `.send()`'s, happens
        // before the real dispatch path.
        mail()
            .to("carol@example.com")
            .queue(Reminder)
            .await
            .unwrap();
        super::assert_sent::<Reminder>(|sent| sent.to == vec!["carol@example.com".to_string()]);
    }
}
