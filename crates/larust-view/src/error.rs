/// A template parsing/resolution error. Deliberately just a message —
/// `larust-macros` is the one place that can turn this into a proper
/// spanned `syn::Error`, and the file/template context is usually already
/// in the message by the time it gets there.
#[derive(Debug)]
pub struct ParseError(String);

impl ParseError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ParseError {}
