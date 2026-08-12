/// Dispatched from `PostController::store` right after a post is created.
/// A plain `Clone` struct is all `larust_support::event::Event` needs —
/// see `docs/ARCHITECTURE.md`'s "Events + Jobs/Queues" section.
#[derive(Clone)]
pub struct PostCreated {
    pub post_id: i64,
    pub title: String,
}
