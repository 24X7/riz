use std::sync::Arc;
use crate::state::AppState;

pub async fn watch_config(_config_path: String, _state: Arc<AppState>) {
    // Full implementation in Task 11
    std::future::pending::<()>().await
}
