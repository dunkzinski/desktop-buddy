//! The plugin's HTTP surface, declared as data.
//!
//! The host asks for [`ROUTES`] once (as JSON) and builds axum routes from it;
//! every request then arrives at [`handle`] as `{method, path, body}`. That is
//! the whole route bridge: the core never names an endpoint, and a plugin can
//! add one without the app being recompiled.

use serde_json::{json, Value};

use crate::model;
use crate::plugin::Survival;

/// The single source of truth for this plugin's endpoints.
///
/// `tools/verify_docs.py` reads this table straight out of the source, so the
/// documented route list follows the guest crate.
pub const ROUTES: &[(&str, &[&str])] = &[
    ("/survival", &["GET", "POST"]),
    ("/survival/feed", &["POST"]),
    ("/foods", &["GET"]),
];

/// `[{"path": "/survival", "methods": ["GET", "POST"]}, …]`
pub fn routes_json() -> Value {
    Value::Array(
        ROUTES
            .iter()
            .map(|(path, methods)| json!({ "path": path, "methods": methods }))
            .collect(),
    )
}

fn ok(body: Value) -> Value {
    json!({ "status": 200, "body": body })
}

fn failed(status: u16, body: Value) -> Value {
    json!({ "status": status, "body": body })
}

/// Dispatch one proxied request.
pub fn handle(state: &mut Survival, request: &Value) -> Value {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_ascii_uppercase();
    // The host strips the query string, but be forgiving.
    let raw_path = request.get("path").and_then(Value::as_str).unwrap_or("/");
    let path = raw_path.split('?').next().unwrap_or("/");
    let body = request.get("body").cloned().unwrap_or(Value::Null);

    match (method.as_str(), path) {
        ("GET", "/survival") => ok(state.status_json()),
        ("POST", "/survival") => {
            let hunger = body.get("hunger").and_then(Value::as_f64).map(|v| v as f32);
            let saturation = body
                .get("saturation")
                .and_then(Value::as_f64)
                .map(|v| v as f32);
            if hunger.is_none() && saturation.is_none() {
                return failed(
                    400,
                    json!({ "ok": false, "error": "send at least one of `hunger` or `saturation`" }),
                );
            }
            match state.set_levels(hunger, saturation) {
                Ok(payload) => ok(payload),
                Err(err) => failed(err.status_code(), err.to_json()),
            }
        }
        ("POST", "/survival/feed") => {
            let food = body
                .get("food")
                .or_else(|| body.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let Some(food) = food else {
                return failed(
                    400,
                    json!({ "ok": false, "error": "send `food` (a catalog id)" }),
                );
            };
            match state.feed(&food) {
                Ok(payload) => ok(payload),
                Err(err) => failed(err.status_code(), err.to_json()),
            }
        }
        ("GET", "/foods") => ok(model::food_catalog()),
        _ => failed(
            404,
            json!({ "ok": false, "error": format!("{method} {path} is not a survival route") }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SurvivalConfig;

    fn plugin() -> Survival {
        // 100 hunger / 60 saturation: satiated, so `cake` is refused.
        Survival::new(SurvivalConfig::default())
    }

    #[test]
    fn the_route_table_is_what_the_host_will_serve() {
        let routes = routes_json();
        let paths: Vec<&str> = routes
            .as_array()
            .expect("array")
            .iter()
            .map(|route| route["path"].as_str().expect("path"))
            .collect();
        assert_eq!(paths, vec!["/survival", "/survival/feed", "/foods"]);
        assert_eq!(routes[0]["methods"], json!(["GET", "POST"]));
    }

    #[test]
    fn get_survival_answers_the_documented_shape() {
        let mut state = plugin();
        let response = handle(&mut state, &json!({ "method": "GET", "path": "/survival" }));
        assert_eq!(response["status"], json!(200));
        let body = &response["body"];
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["enabled"], json!(true));
        assert_eq!(body["hunger"], json!(100.0));
        assert!(body["config"]["metabolism"]["hunger_per_min"].is_number());
        assert!(body["tier"].is_string());
    }

    #[test]
    fn post_survival_without_a_level_is_a_400() {
        let mut state = plugin();
        let response = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival", "body": {} }),
        );
        assert_eq!(response["status"], json!(400));
    }

    #[test]
    fn feeding_an_unknown_food_is_a_400_and_a_full_avatar_is_a_409() {
        let mut state = plugin();
        let unknown = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival/feed", "body": { "food": "pizza" } }),
        );
        assert_eq!(unknown["status"], json!(400));
        assert!(unknown["body"]["error"].as_str().unwrap_or("").contains("pizza"));

        let full = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival/feed", "body": { "food": "cake" } }),
        );
        assert_eq!(full["status"], json!(409), "satiated avatar refuses");
    }

    #[test]
    fn a_missing_food_field_is_a_400() {
        let mut state = plugin();
        let response = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival/feed", "body": {} }),
        );
        assert_eq!(response["status"], json!(400));
    }

    #[test]
    fn the_catalog_is_served_and_an_unknown_path_is_a_404() {
        let mut state = plugin();
        let foods = handle(&mut state, &json!({ "method": "GET", "path": "/foods" }));
        assert_eq!(foods["status"], json!(200));
        assert!(foods["body"]["count"].as_u64().unwrap_or(0) >= 7);

        let missing = handle(&mut state, &json!({ "method": "GET", "path": "/nope" }));
        assert_eq!(missing["status"], json!(404));
    }

    #[test]
    fn a_query_string_is_ignored() {
        let mut state = plugin();
        let response = handle(
            &mut state,
            &json!({ "method": "GET", "path": "/survival?x=1" }),
        );
        assert_eq!(response["status"], json!(200));
    }
}
