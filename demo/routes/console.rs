//! Laravel's `routes/console.php` equivalent - home for schedule
//! declarations. `main.rs`'s `schedule:work` subcommand calls
//! [`schedule`] and hands the result to `larust_support::schedule::work`.

use larust_support::schedule::Schedule;

pub fn schedule() -> Schedule {
    Schedule::new().daily(|| async {
        let count = crate::routes::web::post_count().await?;
        larust_support::tracing::info!(post_count = count, "daily post count (scheduler demo)");
        Ok(())
    })
}
