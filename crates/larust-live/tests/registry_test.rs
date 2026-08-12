//! Pure `LiveRegistry` tests — no session/HTTP involved, mirroring
//! `larust_queue::JobRegistry`'s own test style.

use larust_core::AppError;
use larust_http::session::Session;
use larust_live::{components, LiveComponent};
use larust_view::View;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize)]
struct Fake;

impl LiveComponent for Fake {
    const NAME: &'static str = "duplicate-name-test";

    async fn mount(_session: &Session, _props: &HashMap<String, serde_json::Value>) -> Self {
        Fake
    }

    async fn render(&self) -> View {
        View::new(String::new())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct AlsoFake;

impl LiveComponent for AlsoFake {
    const NAME: &'static str = "duplicate-name-test";

    async fn mount(_session: &Session, _props: &HashMap<String, serde_json::Value>) -> Self {
        AlsoFake
    }

    async fn render(&self) -> View {
        View::new(String::new())
    }

    async fn call(
        &mut self,
        _session: &Session,
        action: &str,
        _args: &serde_json::Value,
    ) -> Result<Option<String>, AppError> {
        Err(AppError::Http {
            status: axum::http::StatusCode::NOT_FOUND,
            message: format!("no action `{action}`"),
        })
    }
}

#[test]
fn registering_two_components_under_the_same_name_panics() {
    // Runs before `.publish()` — the check happens purely inside
    // `register::<C>()` building the map, so this doesn't touch the
    // process-wide registry `OnceLock` at all and is safe to run alongside
    // other tests in this same binary.
    let result =
        std::panic::catch_unwind(|| components().register::<Fake>().register::<AlsoFake>());
    assert!(result.is_err());
}

#[test]
fn registering_distinct_names_does_not_panic() {
    let result = std::panic::catch_unwind(|| {
        #[derive(Debug, Serialize, Deserialize)]
        struct A;
        impl LiveComponent for A {
            const NAME: &'static str = "distinct-a";
            async fn mount(
                _session: &Session,
                _props: &HashMap<String, serde_json::Value>,
            ) -> Self {
                A
            }
            async fn render(&self) -> View {
                View::new(String::new())
            }
        }
        #[derive(Debug, Serialize, Deserialize)]
        struct B;
        impl LiveComponent for B {
            const NAME: &'static str = "distinct-b";
            async fn mount(
                _session: &Session,
                _props: &HashMap<String, serde_json::Value>,
            ) -> Self {
                B
            }
            async fn render(&self) -> View {
                View::new(String::new())
            }
        }

        components().register::<A>().register::<B>()
    });
    assert!(result.is_ok());
}
