use larust_support::console::Command;
use larust_support::AppError;

/// `xr report:posts` - the same post count `routes/console.rs`'s own
/// `.daily(...)` task already logs on a schedule, reachable on demand
/// instead of waiting for the next tick.
pub struct ReportPosts;

impl Command for ReportPosts {
    const NAME: &'static str = "report:posts";
    const DESCRIPTION: &'static str = "Prints the current post count";

    async fn handle(_args: &[String]) -> Result<(), AppError> {
        let count = crate::models::Post::all().await?.len();
        println!("{count} posts");
        Ok(())
    }
}
