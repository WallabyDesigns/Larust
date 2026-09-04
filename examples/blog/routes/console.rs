//! Laravel's `routes/console.php` equivalent - home for schedule
//! declarations. `main.rs`'s `schedule:work` subcommand calls
//! [`schedule`] and hands the result to `larust_support::schedule::work`.

use larust_support::schedule::Schedule;

pub fn schedule() -> Schedule {
    Schedule::new().daily(|| async {
        let count = crate::models::Post::all().await?.len();
        larust_support::tracing::info!(post_count = count, "daily post count (scheduler demo)");
        Ok(())
    })
}
