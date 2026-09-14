//! Laravel's `routes/console.php` equivalent - home for schedule
//! declarations and named CLI commands. `main.rs`'s `schedule:work`/
//! command-dispatch calls [`schedule`]/[`commands`] and hands the result
//! to `larust_support::schedule::work`/`CommandRegistry::dispatch`.

use larust_support::console::CommandRegistry;
use larust_support::schedule::Schedule;

pub fn schedule() -> Schedule {
    Schedule::new().daily(|| async {
        let count = crate::models::Post::all().await?.len();
        larust_support::tracing::info!(post_count = count, "daily post count (scheduler demo)");
        Ok(())
    })
}

pub fn commands() -> CommandRegistry {
    CommandRegistry::new().register::<crate::commands::ReportPosts>()
}
