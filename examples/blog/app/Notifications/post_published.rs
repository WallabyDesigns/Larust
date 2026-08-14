use serde::Serialize;

#[derive(Serialize)]
pub struct PostPublished {
    pub post_id: i64,
    pub title: String,
}

impl larust_support::notification::Notification for PostPublished {
    const NOTIFICATION_TYPE: &'static str = "post_published";
}
