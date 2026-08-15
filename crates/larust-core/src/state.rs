use crate::{AppPaths, Config};
use std::sync::Arc;

/// Explicit, clonable application state for code that should not depend on
/// Larust's legacy process-wide helper facades.
#[derive(Debug, Clone)]
pub struct AppState {
    config: Arc<Config>,
    paths: Arc<AppPaths>,
}

impl AppState {
    pub(crate) fn new(config: Config, paths: AppPaths) -> Self {
        Self {
            config: Arc::new(config),
            paths: Arc::new(paths),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }
}
