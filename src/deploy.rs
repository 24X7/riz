use std::sync::Arc;
use axum::{extract::State, http::StatusCode, response::IntoResponse};
use crate::state::AppState;

pub async fn deploy_handler(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}
