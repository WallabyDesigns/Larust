//! Laravel's `Illuminate\Http\Request`, scoped to what it's for here:
//! reading headers without a handler having to know axum's `HeaderMap`
//! API. Route/query parameters deliberately stay on their own
//! `Path<T>`/`Query<T>` extractors rather than being folded into this
//! type - Laravel's untyped `$request->route('id')` is exactly what
//! `Path<T>`'s compile-time-typed, auto-rejecting alternative already
//! replaces the need for (see `routes/api.rs`'s doc comment).

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::HeaderMap;
use std::collections::BTreeMap;
use std::convert::Infallible;

pub struct Request {
    headers: HeaderMap,
}

impl Request {
    /// Laravel's `$request->header('X-Foo')` - `None` if the header is
    /// absent *or* isn't valid UTF-8 (Laravel's own string-typed `header()`
    /// would choke on that case too, just later and less clearly).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)?.to_str().ok()
    }

    /// Laravel's `$request->headers->all()`, flattened to strings the same
    /// way `header()` is - a non-UTF-8 value falls back to a placeholder
    /// instead of failing the whole map.
    pub fn headers(&self) -> BTreeMap<String, String> {
        self.headers
            .iter()
            .map(|(name, value)| {
                let value = value.to_str().unwrap_or("<non-utf8>").to_string();
                (name.to_string(), value)
            })
            .collect()
    }
}

// GOTCHAS.md: axum-core declares `FromRequestParts` via `#[async_trait]`,
// not native async-fn-in-traits - an impl written as a plain `async fn`
// fails with a confusing E0195 lifetime error instead of a clear message
// about the mismatch.
#[axum::async_trait]
impl<S> FromRequestParts<S> for Request
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Request {
            headers: parts.headers.clone(),
        })
    }
}
