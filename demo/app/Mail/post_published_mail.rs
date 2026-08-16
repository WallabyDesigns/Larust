use larust_support::mail::Mailable;

use crate::models::User;

/// Sent once, right after a post is published (`PostCreated`'s listener in
/// `src/main.rs`) — the second real `Mailable` in this demo, alongside
/// `WelcomeMail`, proving the mail path works for more than just
/// registration. Takes the already-fetched `author` plus the event's own
/// scalar fields rather than a full `Post`, since the listener has no
/// need to re-query the post it already has both pieces of from
/// `PostCreated` itself.
pub struct PostPublishedMail<'a> {
    pub author: &'a User,
    pub post_title: &'a str,
    pub post_id: i64,
}

impl Mailable for PostPublishedMail<'_> {
    fn subject(&self) -> String {
        format!("Your post \"{}\" is live", self.post_title)
    }

    fn html_body(&self) -> String {
        // Absolute, not `/posts/{id}` bare — a relative URL has no
        // meaningful resolution once viewed outside a browser (no "current
        // page" to resolve against), which is exactly why the link
        // rendered broken in a real client. `url()` (M25) is Laravel's own
        // `url()` helper, built for exactly this.
        let post_url = larust_support::url(&format!("/posts/{}", self.post_id));
        larust_support::view!("emails.post_published", {
            name: self.author.name.clone(),
            title: self.post_title.to_string(),
            post_url,
        })
        .into_html()
    }
}
