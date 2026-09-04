//! Proves `Repository<T>` is actually implementable outside SQL, not just
//! plausible on paper - a `HashMap`-backed store with a predicate-closure
//! `Filter` (deliberately not SQL-shaped) stands in for a document store
//! like Firestore. This test binary depends on nothing but `larust-core`/
//! `larust-repository`/`tokio` - no `sqlx` anywhere in its dependency tree.

use larust_core::AppError;
use larust_repository::Repository;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Widget {
    id: i64,
    name: String,
}

struct InMemoryRepository {
    rows: Mutex<HashMap<i64, Widget>>,
    next_id: Mutex<i64>,
}

impl InMemoryRepository {
    fn new() -> Self {
        Self {
            rows: Mutex::new(HashMap::new()),
            next_id: Mutex::new(1),
        }
    }
}

impl Repository<Widget> for InMemoryRepository {
    type Filter = Box<dyn Fn(&Widget) -> bool + Send>;
    type Id = i64;

    async fn find(&self, id: Self::Id) -> Result<Option<Widget>, AppError> {
        Ok(self.rows.lock().unwrap().get(&id).cloned())
    }

    async fn query(&self, filter: Self::Filter) -> Result<Vec<Widget>, AppError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .values()
            .filter(|w| filter(w))
            .cloned()
            .collect())
    }

    async fn create(&self, value: Widget) -> Result<Widget, AppError> {
        let mut next_id = self.next_id.lock().unwrap();
        let id = *next_id;
        *next_id += 1;
        let stored = Widget { id, ..value };
        self.rows.lock().unwrap().insert(id, stored.clone());
        Ok(stored)
    }

    async fn update(&self, id: Self::Id, value: Widget) -> Result<Widget, AppError> {
        let stored = Widget { id, ..value };
        self.rows.lock().unwrap().insert(id, stored.clone());
        Ok(stored)
    }

    async fn delete(&self, id: Self::Id) -> Result<(), AppError> {
        self.rows.lock().unwrap().remove(&id);
        Ok(())
    }
}

#[tokio::test]
async fn in_memory_repository_supports_the_full_crud_round_trip() {
    let repo = InMemoryRepository::new();

    let created = repo
        .create(Widget {
            id: 0,
            name: "widget-a".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(created.name, "widget-a");

    let found = repo.find(created.id).await.unwrap();
    assert_eq!(found, Some(created.clone()));

    let updated = repo
        .update(
            created.id,
            Widget {
                id: 0,
                name: "widget-a-renamed".to_string(),
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.name, "widget-a-renamed");
    assert_eq!(repo.find(created.id).await.unwrap(), Some(updated));

    repo.create(Widget {
        id: 0,
        name: "widget-b".to_string(),
    })
    .await
    .unwrap();

    let matches = repo
        .query(Box::new(|w: &Widget| w.name.starts_with("widget-a")))
        .await
        .unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].name, "widget-a-renamed");

    repo.delete(created.id).await.unwrap();
    assert_eq!(repo.find(created.id).await.unwrap(), None);
}
