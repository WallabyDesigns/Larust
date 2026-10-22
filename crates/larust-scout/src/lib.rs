//! Laravel's `laravel/scout` - full-text search, narrowed to its
//! Meilisearch driver: self-hostable (no required paid service, matching
//! this framework's own posture elsewhere - `larust-reverb`'s own
//! WebSocket server, `larust-db`'s embedded `redb`), with a REST API
//! stable and simple enough to call directly over [`reqwest`] rather than
//! needing a dedicated SDK dependency.
//!
//! **No `Searchable` trait / automatic model-lifecycle indexing.** Real
//! Scout re-indexes a model automatically on save/delete via an Eloquent
//! model observer - this framework has already rejected that shape of
//! magic once, deliberately: `rust-laravel.md`'s own "What should remain
//! deliberately different" section singles out "Magic lazy-loaded
//! relationships by default" as something to reject, and silent
//! lifecycle-hook re-indexing is the same instinct applied to writes
//! instead of reads - a save that also does an unrelated network call the
//! caller never asked for, with no visible line of code at the call site
//! explaining why. [`index`]/[`remove`] are explicit calls instead, the
//! same shape `larust_queue::dispatch`/`larust_events::dispatch` already
//! use for "something happens as a deliberate, visible side effect of
//! this write", not an invisible one.
//!
//! **Single-driver (Meilisearch only), not Scout's own pluggable driver
//! split** (Algolia, a `database`/`collection` driver for local dev and
//! testing). A real `database`-style LIKE-query fallback is a genuinely
//! different, larger feature - this crate has one job, talking to a real
//! Meilisearch instance, and does it without an abstraction layer only
//! one implementation would ever sit behind.
//!
//! Re-exported through `larust_support::scout` (see `crates/
//! larust-support/src/lib.rs`) so generated apps depend only on
//! `larust-support`, never on this crate directly.
//!
//! ## Deliberately out of scope for this version
//!
//! - **No index configuration** (searchable/filterable/sortable attribute
//!   settings, typo tolerance, synonyms - Meilisearch's own `PATCH
//!   /indexes/{uid}/settings`). [`index`] relies entirely on Meilisearch's
//!   own sensible defaults (every field searchable); an app with real
//!   tuning needs calls `reqwest` against the settings endpoint directly
//!   for now.
//! - **No filters, sorting, facets, or pagination parameters** on
//!   [`search`] - plain `q`-only full-text search. A real, separate
//!   follow-up once a caller actually needs them, not spec'd speculatively
//!   here.
//! - **No task-completion polling.** Meilisearch indexes/deletes
//!   asynchronously - [`index`]/[`remove`] return once Meilisearch has
//!   *accepted* the write (its own task-enqueued response), not once
//!   indexing has actually finished. This matches Scout's own real-world
//!   behavior (also eventually consistent, especially when its own queue
//!   driver is active) - not a shortcut unique to this crate.

use larust_core::AppError;
use serde::de::DeserializeOwned;
use serde::Serialize;
use tokio::sync::OnceCell;

/// Set once per process, the first time any Scout function runs - same
/// lazy-singleton shape `larust-cache`'s own `redis_store::connection`
/// already establishes for its `ConnectionManager`. `reqwest::Client`
/// holds its own internal connection pool and is designed to be cloned
/// freely (an `Arc`-backed handle), so every caller gets a cheap clone
/// rather than contending on one shared handle.
static CLIENT: OnceCell<reqwest::Client> = OnceCell::const_new();

async fn client() -> &'static reqwest::Client {
    CLIENT
        .get_or_init(|| async { reqwest::Client::new() })
        .await
}

/// `MEILISEARCH_URL` (default `http://127.0.0.1:7700`) and
/// `MEILISEARCH_KEY` (optional - a local dev instance commonly runs with
/// no master key at all; omitted from the request entirely rather than
/// sent as an empty bearer token when unset). Same "a single plain env
/// var, not a typed config block" reasoning `larust-cache`'s own
/// `redis_store::connection` doc comment already gives for `REDIS_URL` -
/// nothing here needs a driver/dialect choice the way SQL connections do.
fn base_url() -> String {
    std::env::var("MEILISEARCH_URL").unwrap_or_else(|_| "http://127.0.0.1:7700".to_string())
}

fn auth_key() -> Option<String> {
    std::env::var("MEILISEARCH_KEY").ok()
}

fn authorized(mut request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    if let Some(key) = auth_key() {
        request = request.bearer_auth(key);
    }
    request
}

/// Indexes `document` into `index_name` - Laravel's `$model->searchable()`.
/// `document` must serialize to a JSON object carrying an `"id"` field
/// (Meilisearch's own default primary-key convention, and the same `id`
/// every `#[derive(Model)]` struct already has) - this crate adds no
/// separate id parameter or primary-key configuration on top of that.
/// Meilisearch creates `index_name` automatically on this first write if
/// it doesn't already exist; no separate "create the index" call is
/// needed.
pub async fn index<T: Serialize>(index_name: &str, document: &T) -> Result<(), AppError> {
    let url = format!("{}/indexes/{index_name}/documents", base_url());
    // A single document still goes through Meilisearch's own bulk-add
    // endpoint, which only ever accepts a JSON array - wrapped here so
    // callers never need to know that.
    let body = serde_json::json!([document]);
    let response = authorized(client().await.post(&url).json(&body))
        .send()
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    ensure_success(response, "indexing a document").await
}

/// Removes the document with id `id` from `index_name` - Laravel's
/// `$model->unsearchable()`. Not an error to remove an id that was never
/// indexed or has already been removed - Meilisearch itself treats that
/// as a normal, successful no-op (it still enqueues a delete task; there
/// is nothing to roll back).
pub async fn remove(index_name: &str, id: impl std::fmt::Display) -> Result<(), AppError> {
    let url = format!("{}/indexes/{index_name}/documents/{id}", base_url());
    let response = authorized(client().await.delete(&url))
        .send()
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;
    ensure_success(response, "removing a document").await
}

/// One page of search results - Meilisearch's own default (offset-based,
/// not the alternate page-number mode its own API also supports, see this
/// module's own "deliberately out of scope" note on pagination
/// parameters). `estimated_total_hits` is `None` only for the "index
/// doesn't exist yet" fallback [`search`] itself applies - a real
/// Meilisearch response always includes it.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SearchResults<T> {
    pub hits: Vec<T>,
    #[serde(rename = "estimatedTotalHits")]
    pub estimated_total_hits: Option<u64>,
}

/// Searches `index_name` for `query` - Laravel's `Model::search($query)
/// ->get()`. An index that has never had a document written to it (see
/// [`index`]'s own doc comment - Meilisearch never auto-creates one for a
/// search, only for a write) returns Meilisearch's own `index_not_found`
/// error; treated here as an ordinary empty result set, not a caller-
/// visible error, the same "nothing indexed yet looks like zero results,
/// not a failure" tolerance an empty SQL table already gets everywhere
/// else in this codebase.
pub async fn search<T: DeserializeOwned>(
    index_name: &str,
    query: &str,
) -> Result<SearchResults<T>, AppError> {
    let url = format!("{}/indexes/{index_name}/search", base_url());
    let body = serde_json::json!({ "q": query });
    let response = authorized(client().await.post(&url).json(&body))
        .send()
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(SearchResults {
            hits: Vec::new(),
            estimated_total_hits: Some(0),
        });
    }
    let response = ensure_status(response, "searching").await?;
    response
        .json()
        .await
        .map_err(|source| AppError::Internal(Box::new(source)))
}

/// Shared status-check behind [`index`]/[`remove`] - both only care
/// whether the request succeeded, never the response body itself
/// (Meilisearch's own enqueued-task object).
async fn ensure_success(response: reqwest::Response, action: &str) -> Result<(), AppError> {
    ensure_status(response, action).await.map(|_| ())
}

/// Turns a non-2xx Meilisearch response into a real `AppError`, with the
/// response body included (Meilisearch's own JSON error responses always
/// carry a human-readable `message` field) rather than just the bare
/// status code - the same "surface the real failure, don't make the
/// caller go dig through logs" instinct this codebase's other external-
/// service-calling crates already follow.
async fn ensure_status(
    response: reqwest::Response,
    action: &str,
) -> Result<reqwest::Response, AppError> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    Err(AppError::Internal(Box::new(std::io::Error::other(
        format!("Meilisearch request failed while {action} ({status}): {body}"),
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Path;
    use axum::response::IntoResponse;
    use axum::routing::{delete, post};
    use axum::{Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[derive(Debug, Clone, Default)]
    struct Hits {
        index: Arc<AtomicUsize>,
        remove: Arc<AtomicUsize>,
        search: Arc<AtomicUsize>,
    }

    /// A real local HTTP server standing in for Meilisearch - same
    /// "no mocking crate, a tiny real `axum` server on an ephemeral port"
    /// pattern `larust-socialite`'s own test suite already establishes
    /// for the identical problem (no real external service available in
    /// this environment to test against).
    async fn start_mock_meilisearch() -> (String, Hits) {
        let hits = Hits::default();

        let index_hits = hits.index.clone();
        let search_hits = hits.search.clone();
        let app = Router::new()
            .route(
                "/indexes/:index/documents",
                post(move |Json(_body): Json<serde_json::Value>| {
                    let index_hits = index_hits.clone();
                    async move {
                        index_hits.fetch_add(1, Ordering::SeqCst);
                        Json(serde_json::json!({"taskUid": 1, "status": "enqueued"}))
                    }
                }),
            )
            .route(
                "/indexes/:index/documents/:id",
                delete({
                    let remove_hits = hits.remove.clone();
                    move |Path((_index, _id)): Path<(String, String)>| {
                        let remove_hits = remove_hits.clone();
                        async move {
                            remove_hits.fetch_add(1, Ordering::SeqCst);
                            Json(serde_json::json!({"taskUid": 2, "status": "enqueued"}))
                        }
                    }
                }),
            )
            .route(
                "/indexes/:index/search",
                post(
                    move |Path(index): Path<String>, Json(_body): Json<serde_json::Value>| {
                        let search_hits = search_hits.clone();
                        async move {
                            search_hits.fetch_add(1, Ordering::SeqCst);
                            if index == "empty-index" {
                                return axum::http::StatusCode::NOT_FOUND.into_response();
                            }
                            Json(serde_json::json!({
                                "hits": [{"id": 1, "title": "a matching post"}],
                                "estimatedTotalHits": 1,
                            }))
                            .into_response()
                        }
                    },
                ),
            );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), hits)
    }

    #[derive(Debug, serde::Deserialize)]
    struct Post {
        #[allow(dead_code)]
        id: i64,
        title: String,
    }

    /// All scenarios share one test function, not several - `MEILISEARCH_URL`
    /// is read via `std::env::var` on every call (not memoized), but the
    /// mock server itself is still process-wide state worth standing up
    /// only once, same "one shared fixture, one test" shape this
    /// codebase's own process-wide-singleton test suites already use
    /// elsewhere (`larust-sanctum`/`larust-permissions`/
    /// `larust-notifications`).
    #[tokio::test]
    async fn scout_crate_behaves_correctly_across_every_scenario() {
        let (base_url, hits) = start_mock_meilisearch().await;
        // SAFETY: this test is the only thing in this process that ever
        // sets `MEILISEARCH_URL` - no other test in this crate reads or
        // writes it concurrently.
        unsafe {
            std::env::set_var("MEILISEARCH_URL", &base_url);
        }

        index("posts", &serde_json::json!({"id": 1, "title": "hello"}))
            .await
            .unwrap();
        assert_eq!(hits.index.load(Ordering::SeqCst), 1);

        remove("posts", 1).await.unwrap();
        assert_eq!(hits.remove.load(Ordering::SeqCst), 1);

        let results: SearchResults<Post> = search("posts", "hello").await.unwrap();
        assert_eq!(hits.search.load(Ordering::SeqCst), 1);
        assert_eq!(results.hits.len(), 1);
        assert_eq!(results.hits[0].title, "a matching post");
        assert_eq!(results.estimated_total_hits, Some(1));

        // An index that doesn't exist yet reads as zero results, not an
        // error - see `search`'s own doc comment for why.
        let empty: SearchResults<Post> = search("empty-index", "anything").await.unwrap();
        assert!(empty.hits.is_empty());
        assert_eq!(empty.estimated_total_hits, Some(0));
    }
}
