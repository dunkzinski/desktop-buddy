//! The plugin's HTTP surface.
//!
//! These handlers are returned to the host as [`PluginRoute`]s, which the API
//! merges into its router. `api.rs` therefore contains no survival path, no
//! survival body struct and no survival status code — dropping this file drops
//! the four endpoints with it.
//!
//! Every handler is a closure over the plugin's [`Handle`], so it reaches its
//! state without a global, a service locator or a host lookup.

use std::sync::Arc;

use axum::extract::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use super::model;
use super::plugin::{FeedError, Handle};
use crate::plugins::PluginRoute;

#[derive(Debug, Deserialize)]
struct SurvivalBody {
    /// New hunger level, 0–100. Absent ⇒ leave it alone.
    #[serde(default)]
    hunger: Option<f32>,
    /// New saturation level, 0–100. Absent ⇒ leave it alone.
    #[serde(default)]
    saturation: Option<f32>,
}

#[derive(Debug, Deserialize)]
struct FeedBody {
    /// Catalog id, e.g. `"apple"`. `id` is accepted as an alias so an agent
    /// that read the catalog's `id` field can round-trip it without guessing.
    #[serde(default)]
    food: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

/// A refusal is a real answer: 400 for "not installed"/"no such food", 409 for
/// "the avatar said no", both carrying `{ok:false, error}` like the rest of the
/// API.
impl IntoResponse for FeedError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status_code()).unwrap_or(StatusCode::BAD_REQUEST);
        (status, Json(json!({ "ok": false, "error": self.to_string() }))).into_response()
    }
}

/// `GET /survival`, `POST /survival`, `POST /survival/feed`, `GET /foods`.
pub(crate) fn routes(state: &Handle) -> Vec<PluginRoute> {
    let get_state = Arc::clone(state);
    let post_state = Arc::clone(state);
    let feed_state = Arc::clone(state);

    vec![
        PluginRoute::get_post(
            "/survival",
            move || {
                let state = Arc::clone(&get_state);
                async move { Json(state.status_json()) }
            },
            move |Json(body): Json<SurvivalBody>| {
                let state = Arc::clone(&post_state);
                async move {
                    if body.hunger.is_none() && body.saturation.is_none() {
                        return Err(FeedError::Refused(json!({
                            "error": "send at least one of `hunger` or `saturation`",
                        })));
                    }
                    state
                        .set_levels(body.hunger, body.saturation)
                        .map(Json)
                }
            },
        ),
        PluginRoute::post("/survival/feed", move |Json(body): Json<FeedBody>| {
            let state = Arc::clone(&feed_state);
            async move {
                let food = body
                    .food
                    .or(body.id)
                    .ok_or_else(|| FeedError::UnknownFood("send `food` (a catalog id)".to_string()))?;
                state.feed(&food).map(Json)
            }
        }),
        PluginRoute::get("/foods", || async { Json(model::food_catalog()) }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    // The handlers are only *built* here: calling one needs a real
    // `crate::api::ApiCtx`, which carries a Tauri `AppHandle` and cannot exist
    // in a unit test. The behaviour behind each handler
    // (`status_json`/`feed`/`set_levels`) is exercised in `plugin::tests`, and
    // the live wiring is proven end-to-end against the running app.
    #[test]
    fn the_plugin_contributes_its_endpoints() {
        let state: Handle = Arc::new(super::super::plugin::SurvivalState::for_tests());
        let paths: Vec<&str> = routes(&state).iter().map(|route| route.path).collect();
        assert_eq!(paths, vec!["/survival", "/survival/feed", "/foods"]);
    }

    #[test]
    fn a_refusal_maps_to_its_documented_status() {
        assert_eq!(FeedError::NotInstalled.status_code(), 400);
        assert_eq!(FeedError::UnknownFood("x".into()).status_code(), 400);
        assert_eq!(FeedError::Refused(Value::Null).status_code(), 409);
    }
}
