//! End-to-end proof that `#[observe(...)]` hooks actually fire, in order,
//! around the real generated `create`/`update`/`delete` codegen, against
//! a real SQLite database - mirrors `model.rs`'s own
//! `model_crud_round_trip_against_real_sqlite` in shape.

use larust_support::AppError;
use larust_support::Model;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

static CALLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static VETO_CREATE: AtomicUsize = AtomicUsize::new(0);

fn record(event: &str) {
    CALLS.lock().unwrap().push(event.to_string());
}

#[derive(Model, sqlx::FromRow, Debug, PartialEq, Clone)]
#[table("posts")]
#[soft_deletes]
#[observe(
    creating = PostObserver::creating,
    created = PostObserver::created,
    updating = PostObserver::updating,
    updated = PostObserver::updated,
    deleting = PostObserver::deleting,
    deleted = PostObserver::deleted
)]
pub struct Post {
    #[primary_key]
    pub id: i64,
    pub title: String,
    pub deleted_at: Option<i64>,
}

struct PostObserver;

impl PostObserver {
    async fn creating(data: NewPost) -> Result<NewPost, AppError> {
        record("creating");
        if VETO_CREATE.load(Ordering::SeqCst) == 1 {
            return Err(AppError::Http {
                status: larust_support::axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                message: "vetoed by creating hook".to_string(),
            });
        }
        Ok(NewPost {
            title: data.title.to_uppercase(),
        })
    }

    async fn created(post: &Post) -> Result<(), AppError> {
        record(&format!("created:{}", post.title));
        Ok(())
    }

    async fn updating(id: i64, data: NewPost) -> Result<NewPost, AppError> {
        record(&format!("updating:{id}"));
        Ok(data)
    }

    async fn updated(post: &Post) -> Result<(), AppError> {
        record(&format!("updated:{}", post.title));
        Ok(())
    }

    async fn deleting(post: &Post) -> Result<(), AppError> {
        record(&format!("deleting:{}", post.title));
        Ok(())
    }

    async fn deleted(post: &Post) -> Result<(), AppError> {
        record(&format!("deleted:{}", post.title));
        Ok(())
    }
}

#[tokio::test]
async fn observer_hooks_fire_in_order_around_the_real_generated_codegen() {
    let db_dir = tempfile::tempdir().unwrap();
    let database_url = format!("sqlite://{}/test.sqlite", db_dir.path().display());
    larust_support::orm::connect(&database_url).await.unwrap();

    let migrations_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        migrations_dir.path().join("0001_create_posts.sql"),
        "CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, deleted_at INTEGER);",
    )
    .unwrap();
    larust_support::orm::migrate(migrations_dir.path())
        .await
        .unwrap();

    // `creating` transforms the data (uppercases the title) before the
    // INSERT runs - proven structurally: the persisted row reflects the
    // transformed value, not the caller's original one.
    let created = Post::create(NewPost {
        title: "hello".to_string(),
    })
    .await
    .unwrap();
    assert_eq!(created.title, "HELLO");
    assert_eq!(
        *CALLS.lock().unwrap(),
        vec!["creating".to_string(), "created:HELLO".to_string()]
    );
    CALLS.lock().unwrap().clear();

    // `creating` can veto the write entirely - no row is inserted, and
    // `created` never fires.
    VETO_CREATE.store(1, Ordering::SeqCst);
    let veto_result = Post::create(NewPost {
        title: "nope".to_string(),
    })
    .await;
    assert!(veto_result.is_err());
    assert_eq!(*CALLS.lock().unwrap(), vec!["creating".to_string()]);
    assert_eq!(
        Post::all().await.unwrap().len(),
        1,
        "the vetoed row must not exist"
    );
    VETO_CREATE.store(0, Ordering::SeqCst);
    CALLS.lock().unwrap().clear();

    // updating/updated fire around update(), in order.
    let updated = Post::update(
        created.id,
        NewPost {
            title: "world".to_string(),
        },
    )
    .await
    .unwrap();
    assert_eq!(updated.title, "world");
    assert_eq!(
        *CALLS.lock().unwrap(),
        vec![
            format!("updating:{}", created.id),
            "updated:world".to_string()
        ]
    );
    CALLS.lock().unwrap().clear();

    // deleting/deleted fire around delete() (soft-delete, per
    // #[soft_deletes] above), with the pre-fetched snapshot - both see
    // the row's state *before* it was trashed.
    Post::delete(created.id).await.unwrap();
    assert_eq!(
        *CALLS.lock().unwrap(),
        vec!["deleting:world".to_string(), "deleted:world".to_string()]
    );
    CALLS.lock().unwrap().clear();

    // Deleting an id that never existed is still not an error, and no
    // hook fires for it - nothing to observe.
    Post::delete(999_999).await.unwrap();
    assert!(CALLS.lock().unwrap().is_empty());
}
