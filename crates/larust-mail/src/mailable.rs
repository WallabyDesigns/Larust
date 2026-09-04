/// A single email type (Laravel's Mailable classes) - implemented once
/// per email the app sends. Both methods are required, with no default
/// body: a trait-level default would reintroduce the same "silent gap
/// instead of compile error" failure mode `Policy<U>`'s own doc comment
/// already treats as worth avoiding - forgetting to write a real subject
/// or body should fail to compile, not silently send a blank email.
///
/// A typical implementation renders its body through the same `view!`
/// macro used for HTTP responses, via `View::into_html()`:
///
/// ```ignore
/// pub struct WelcomeMail<'a> { pub user: &'a User }
///
/// impl larust_support::mail::Mailable for WelcomeMail<'_> {
///     fn subject(&self) -> String {
///         format!("Welcome, {}!", self.user.name)
///     }
///     fn html_body(&self) -> String {
///         larust_support::view!("emails.welcome", { name: self.user.name.clone() })
///             .into_html()
///     }
/// }
/// ```
pub trait Mailable {
    fn subject(&self) -> String;
    fn html_body(&self) -> String;
}
