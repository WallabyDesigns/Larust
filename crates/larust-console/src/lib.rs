//! Named CLI commands - Laravel's `Artisan::command('name', $closure)`,
//! as a real trait rather than a closure, matching this codebase's own
//! established shape for anything dispatched by a stable string tag
//! (`larust_queue::Job::JOB_TYPE`, `larust_notifications::Notification::
//! NOTIFICATION_TYPE`) rather than introducing a different shape just for
//! this one case.
//!
//! Deliberately **not** built on `larust_queue::JobRegistry`'s exact
//! mechanics, even though the shape looks similar at a glance: a `Job` is
//! serialized, persisted, and reconstructed by a worker process that may
//! not be the one that dispatched it - `JobRegistry::register::<J>()`'s
//! closure exists specifically to defer *deserializing* `J` until a
//! worker actually claims one. A `Command` runs synchronously, in the
//! same process, the moment its name is typed - there's nothing to
//! persist or deserialize, so `register` takes no `Default`/`Deserialize`
//! bound at all, and `Command::handle` is an associated function (no
//! `&self`) rather than an instance method, since a command has no
//! meaningful state of its own beyond the `args` it's handed - the same
//! reasoning `Authenticatable::find_for_auth` being a bare associated
//! function already established for this codebase.
//!
//! # Example
//!
//! ```
//! use larust_console::{Command, CommandRegistry};
//! use larust_core::AppError;
//!
//! struct ReportPosts;
//!
//! impl Command for ReportPosts {
//!     const NAME: &'static str = "report:posts";
//!     const DESCRIPTION: &'static str = "Prints the current post count";
//!
//!     async fn handle(_args: &[String]) -> Result<(), AppError> {
//!         println!("42 posts");
//!         Ok(())
//!     }
//! }
//!
//! # async fn example() -> Result<(), AppError> {
//! let registry = CommandRegistry::new().register::<ReportPosts>();
//! let ran = registry.dispatch("report:posts", &[]).await?;
//! assert!(ran);
//! # Ok(())
//! # }
//! ```

use larust_core::AppError;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

/// A named CLI command, run once, synchronously, in the same process that
/// dispatched it - see this crate's own module doc for the full
/// comparison against `larust_queue::Job`.
pub trait Command: Send + Sync + 'static {
    /// The name typed after `xr` to run this command, e.g. `xr
    /// report:posts` names a command whose `NAME` is `"report:posts"`.
    /// Colon-namespaced by convention (Laravel's own `make:model`,
    /// `queue:work`), never enforced - any string works.
    const NAME: &'static str;

    /// A one-line description, shown by `xr command:list`. Optional -
    /// defaults to empty rather than requiring every command to supply
    /// one, since a name alone is often self-explanatory.
    const DESCRIPTION: &'static str = "";

    /// Runs the command. `args` is every token after the command's own
    /// name (`xr report:posts --format=json` hands `["--format=json"]`) -
    /// parsing them is the command's own job, the same "no
    /// framework-imposed argument-parsing DSL" stance `Job`/`Notification`
    /// already take for their own payloads.
    fn handle(args: &[String]) -> impl Future<Output = Result<(), AppError>> + Send;
}

type BoxedHandler = Box<
    dyn Fn(Vec<String>) -> Pin<Box<dyn Future<Output = Result<(), AppError>> + Send>> + Send + Sync,
>;

/// A registered command's name and description, as returned by
/// [`CommandRegistry::list`] - e.g. for `xr command:list` to print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandInfo {
    pub name: &'static str,
    pub description: &'static str,
}

/// Maps a command name to the handler that runs it - built fresh by
/// whatever calls [`CommandRegistry::dispatch`] (typically the generated
/// app's own `main.rs`, via `routes::console::commands()`), not a
/// process-wide static: exactly one process ever reads it, for exactly
/// as long as that one `xr <command>` invocation runs, so there's no
/// cross-request sharing need the way `larust-events`' listener registry
/// or `larust_http::route`'s named-route registry have.
#[must_use]
#[derive(Default)]
pub struct CommandRegistry {
    commands: HashMap<&'static str, (CommandInfo, BoxedHandler)>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `C` under its own `Command::NAME`. Panics on a duplicate
    /// name - a startup-time registration collision is a real programmer
    /// bug (the shadowed command would silently never run again); failing
    /// loudly and immediately here beats a confusing "why did `xr
    /// report:posts` just run the wrong thing" bug report later, the same
    /// reasoning `JobRegistry::register`'s own duplicate check already
    /// established for this codebase.
    pub fn register<C: Command>(mut self) -> Self {
        let handler: BoxedHandler =
            Box::new(|args: Vec<String>| Box::pin(async move { C::handle(&args).await }));
        let info = CommandInfo {
            name: C::NAME,
            description: C::DESCRIPTION,
        };
        let existing = self.commands.insert(C::NAME, (info, handler));
        assert!(
            existing.is_none(),
            "duplicate CommandRegistry::register for NAME {:?} - each command name must be \
             registered exactly once",
            C::NAME,
        );
        self
    }

    /// Runs the named command with `args`, returning `Ok(true)` if a
    /// command with that name was actually registered and run, or
    /// `Ok(false)` if nothing matched. `false`, not an error - the
    /// distinction lets a caller (`main.rs`'s own dispatch chain) fall
    /// through to its own "unknown command" handling for a name that
    /// matches neither a registered `Command` nor one of the framework's
    /// own fixed subcommands (`migrate`, `queue:work`, ...), rather than
    /// this crate having to know about those at all.
    pub async fn dispatch(&self, name: &str, args: &[String]) -> Result<bool, AppError> {
        match self.commands.get(name) {
            Some((_, handler)) => {
                handler(args.to_vec()).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Every registered command's name and description, sorted by name -
    /// for `xr command:list` to print in a stable, predictable order
    /// rather than `HashMap`'s own unspecified iteration order.
    pub fn list(&self) -> Vec<CommandInfo> {
        let mut commands: Vec<CommandInfo> =
            self.commands.values().map(|(info, _)| *info).collect();
        commands.sort_by_key(|info| info.name);
        commands
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Ping;
    impl Command for Ping {
        const NAME: &'static str = "ping";
        const DESCRIPTION: &'static str = "Replies pong";

        async fn handle(_args: &[String]) -> Result<(), AppError> {
            Ok(())
        }
    }

    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static LAST_ARGS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    struct Echo;
    impl Command for Echo {
        const NAME: &'static str = "echo";

        async fn handle(args: &[String]) -> Result<(), AppError> {
            CALLS.fetch_add(1, Ordering::SeqCst);
            *LAST_ARGS.lock().unwrap() = args.to_vec();
            Ok(())
        }
    }

    struct Failing;
    impl Command for Failing {
        const NAME: &'static str = "failing";

        async fn handle(_args: &[String]) -> Result<(), AppError> {
            Err(AppError::Internal(Box::new(std::io::Error::other(
                "deliberately failing",
            ))))
        }
    }

    #[tokio::test]
    async fn dispatch_runs_the_matching_registered_command() {
        let registry = CommandRegistry::new().register::<Ping>();
        let ran = registry.dispatch("ping", &[]).await.unwrap();
        assert!(ran);
    }

    #[tokio::test]
    async fn dispatch_forwards_args_to_the_command() {
        CALLS.store(0, Ordering::SeqCst);
        let registry = CommandRegistry::new().register::<Echo>();
        let args = vec!["--dry-run".to_string(), "42".to_string()];
        registry.dispatch("echo", &args).await.unwrap();
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        assert_eq!(*LAST_ARGS.lock().unwrap(), args);
    }

    #[tokio::test]
    async fn dispatch_returns_false_for_an_unregistered_name() {
        let registry = CommandRegistry::new().register::<Ping>();
        let ran = registry.dispatch("not:registered", &[]).await.unwrap();
        assert!(!ran);
    }

    #[tokio::test]
    async fn dispatch_propagates_the_commands_own_error() {
        let registry = CommandRegistry::new().register::<Failing>();
        let result = registry.dispatch("failing", &[]).await;
        assert!(result.is_err());
    }

    #[test]
    #[should_panic(expected = "duplicate CommandRegistry::register for NAME \"ping\"")]
    fn register_panics_on_a_duplicate_name() {
        struct AlsoPing;
        impl Command for AlsoPing {
            const NAME: &'static str = "ping";
            async fn handle(_args: &[String]) -> Result<(), AppError> {
                Ok(())
            }
        }

        let _ = CommandRegistry::new()
            .register::<Ping>()
            .register::<AlsoPing>();
    }

    #[test]
    fn list_is_sorted_by_name_regardless_of_registration_order() {
        struct Zebra;
        impl Command for Zebra {
            const NAME: &'static str = "zebra";
            async fn handle(_args: &[String]) -> Result<(), AppError> {
                Ok(())
            }
        }
        struct Apple;
        impl Command for Apple {
            const NAME: &'static str = "apple";
            async fn handle(_args: &[String]) -> Result<(), AppError> {
                Ok(())
            }
        }

        let registry = CommandRegistry::new()
            .register::<Zebra>()
            .register::<Apple>();
        let names: Vec<&str> = registry.list().iter().map(|info| info.name).collect();
        assert_eq!(names, vec!["apple", "zebra"]);
    }

    #[test]
    fn list_includes_each_commands_description() {
        let registry = CommandRegistry::new().register::<Ping>();
        let list = registry.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "ping");
        assert_eq!(list[0].description, "Replies pong");
    }
}
