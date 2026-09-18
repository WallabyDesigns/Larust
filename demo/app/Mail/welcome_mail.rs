use larust_support::mail::Mailable;

use crate::models::User;

/// Sent once, right after a successful registration
/// (`AuthController::register`).
pub struct WelcomeMail<'a> {
    pub user: &'a User,
}

impl Mailable for WelcomeMail<'_> {
    fn subject(&self) -> String {
        larust_support::lang::t_with("email.welcome_subject", &[("name", &self.user.name)])
    }

    fn html_body(&self) -> String {
        larust_support::view!("emails.welcome", { name: self.user.name.clone() }).into_html()
    }
}
