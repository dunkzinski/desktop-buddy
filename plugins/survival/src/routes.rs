//! The plugin's HTTP surface, declared as data.
//!
//! The host asks for [`ROUTES`] once (as JSON) and builds axum routes from it;
//! every request then arrives at [`handle`] as `{method, path, body}`. That is
//! the whole route bridge: the core never names an endpoint, and a plugin can
//! add one without the app being recompiled.

use serde_json::{json, Value};

use crate::inventory;
use crate::model;
use crate::plugin::Survival;

/// The single source of truth for this plugin's endpoints.
///
/// `tools/verify_docs.py` reads this table straight out of the source, so the
/// documented route list follows the guest crate.
pub const ROUTES: &[(&str, &[&str])] = &[
    ("/survival", &["GET", "POST"]),
    ("/survival/feed", &["POST"]),
    ("/survival/craft", &["POST"]),
    ("/inventory", &["GET"]),
    ("/inventory/add", &["POST"]),
    ("/inventory/remove", &["POST"]),
    ("/inventory/use", &["POST"]),
    ("/fishing", &["GET"]),
    ("/fishing/cast", &["POST"]),
    ("/fishing/hook", &["POST"]),
    ("/fishing/reel", &["POST"]),
    ("/fishing/cancel", &["POST"]),
    ("/foods", &["GET"]),
    ("/recipes", &["GET"]),
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

/// Read an ingredient payload in any of the three accepted shapes:
/// `["egg", "egg", "cheese"]`, `{"egg": 2, "cheese": 1}` or a bare `"omelette"`.
fn parse_ingredients(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(item.as_str()?.to_string());
            }
            Some(out)
        }
        Value::Object(counts) => {
            let mut out = Vec::new();
            for (id, count) in counts {
                // A recipe never needs more than a handful of anything, and a
                // typo like 1000 must not build a 1000-element vector.
                let count = count.as_u64()?.clamp(1, 9) as usize;
                for _ in 0..count {
                    out.push(id.clone());
                }
            }
            Some(out)
        }
        Value::String(id) => Some(vec![id.clone()]),
        _ => None,
    }
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
            // The same endpoint carries bar overrides and the craving control,
            // in any combination; `craving: null` clears it.
            let hunger = body.get("hunger").and_then(Value::as_f64).map(|v| v as f32);
            let saturation = body
                .get("saturation")
                .and_then(Value::as_f64)
                .map(|v| v as f32);
            let has_craving = body.get("craving").is_some();
            if hunger.is_none() && saturation.is_none() && !has_craving {
                return failed(
                    400,
                    json!({
                        "ok": false,
                        "error": "send at least one of `hunger`, `saturation` or `craving`",
                    }),
                );
            }
            if hunger.is_some() || saturation.is_some() {
                if let Err(err) = state.set_levels(hunger, saturation) {
                    return failed(err.status_code(), err.to_json());
                }
            }
            if let Some(spec) = body.get("craving") {
                if let Err(err) = state.set_craving(spec) {
                    return failed(err.status_code(), err.to_json());
                }
            }
            ok(state.status_json())
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
        ("POST", "/survival/craft") => {
            // Either name the recipe, or hand over the ingredients and let the
            // recipe book decide which dish that is.
            if let Some(recipe) = body.get("recipe").and_then(Value::as_str) {
                return match state.craft(recipe) {
                    Ok(payload) => ok(payload),
                    Err(err) => failed(err.status_code(), err.to_json()),
                };
            }
            let Some(offered) = body.get("ingredients") else {
                return failed(
                    400,
                    json!({
                        "ok": false,
                        "error": "send `recipe` (an id from GET /recipes) or `ingredients` \
                                  ([\"egg\",\"egg\",\"cheese\"] or {\"egg\":2,\"cheese\":1})",
                    }),
                );
            };
            let Some(ingredients) = parse_ingredients(offered) else {
                return failed(
                    400,
                    json!({
                        "ok": false,
                        "error": "`ingredients` must be an array of ids or an object of id → count",
                    }),
                );
            };
            match state.craft_from_ingredients(&ingredients) {
                Ok(payload) => ok(payload),
                Err(err) => failed(err.status_code(), err.to_json()),
            }
        }
        ("GET", "/foods") => ok(model::food_catalog()),
        ("GET", "/recipes") => ok(model::recipe_catalog()),
        ("GET", "/inventory") => ok(state.inventory_json()),
        ("POST", "/inventory/add") | ("POST", "/inventory/remove") => {
            // `{"item": "minnow", "count": 3}`, with `id` accepted as a synonym
            // and `count` defaulting to 1 — the boring shapes are the ones a
            // human types by hand.
            let Some(item) = body
                .get("item")
                .or_else(|| body.get("id"))
                .and_then(Value::as_str)
            else {
                return failed(
                    400,
                    json!({
                        "ok": false,
                        "error": "send `item` (an id from GET /inventory's catalog) and optionally `count`",
                    }),
                );
            };
            let count = body
                .get("count")
                .and_then(Value::as_u64)
                .map(|value| value.clamp(1, u64::from(inventory::MAX_ADD)) as u32)
                .unwrap_or(1);
            let adding = path == "/inventory/add";
            let outcome = if adding {
                state.inventory_add(item, count)
            } else {
                state.inventory_remove(item, count)
            };
            match outcome {
                Ok(payload) => ok(payload),
                Err(err) => failed(err.status_code(), err.to_json()),
            }
        }
        ("POST", "/inventory/use") => {
            // `{"item": "silver_trout"}`, `{"id": …}` or `{"slot": 3}`: a slot
            // number is how the grid's click handler addresses an item.
            let mut item = body
                .get("item")
                .or_else(|| body.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            if item.is_none() {
                if let Some(slot) = body.get("slot").and_then(Value::as_u64) {
                    item = state
                        .inventory_json()
                        .get("slots")
                        .and_then(Value::as_array)
                        .and_then(|slots| slots.get(slot as usize))
                        .and_then(|entry| entry.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                }
            }
            let Some(item) = item else {
                return failed(
                    400,
                    json!({
                        "ok": false,
                        "error": "send `item` (an id), `id` or `slot` (an index from GET /inventory)",
                    }),
                );
            };
            match state.use_item(&item) {
                Ok(payload) => ok(payload),
                Err(err) => failed(err.status_code(), err.to_json()),
            }
        }
        ("GET", "/fishing") => ok(state.fishing_status()),
        ("POST", "/fishing/cast") => match state.cast() {
            Ok(payload) => ok(payload),
            Err(err) => failed(err.status_code(), err.to_json()),
        },
        ("POST", "/fishing/hook") => match state.hook() {
            Ok(payload) => ok(payload),
            Err(err) => failed(err.status_code(), err.to_json()),
        },
        ("POST", "/fishing/reel") => match state.reel() {
            Ok(payload) => ok(payload),
            Err(err) => failed(err.status_code(), err.to_json()),
        },
        ("POST", "/fishing/cancel") => match state.cancel_fishing() {
            Ok(payload) => ok(payload),
            Err(err) => failed(err.status_code(), err.to_json()),
        },
        _ => failed(
            404,
            json!({ "ok": false, "error": format!("{method} {path} is not a survival route") }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::take_recorded;
    use crate::model::SurvivalConfig;

    fn plugin() -> Survival {
        // 100 hunger / 60 saturation: satiated, so `cake` is refused.
        Survival::new(SurvivalConfig::default())
    }

    /// A plugin that is hungry enough to accept a meal.
    fn hungry_plugin() -> Survival {
        let mut state = Survival::new(SurvivalConfig::default());
        state.set_levels(Some(40.0), Some(0.0)).expect("override");
        take_recorded();
        state
    }

    /// Move the (test) host clock forward so a meal cooldown has elapsed.
    fn advance_clock(ms: u64) {
        crate::host::set_now_ms(crate::host::now_ms() + ms);
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
        assert_eq!(
            paths,
            vec![
                "/survival",
                "/survival/feed",
                "/survival/craft",
                "/inventory",
                "/inventory/add",
                "/inventory/remove",
                "/inventory/use",
                "/fishing",
                "/fishing/cast",
                "/fishing/hook",
                "/fishing/reel",
                "/fishing/cancel",
                "/foods",
                "/recipes"
            ]
        );
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
        assert!(body["craving"].is_null());
        assert!(body["config"]["cravings"].is_boolean());
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
    fn post_survival_can_install_and_clear_a_craving() {
        let mut state = hungry_plugin();
        let set = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival", "body": { "craving": "omelette" } }),
        );
        assert_eq!(set["status"], json!(200));
        assert_eq!(set["body"]["craving"]["target"], "omelette");
        assert_eq!(set["body"]["craving"]["kind"], "food");

        let cleared = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival", "body": { "craving": null } }),
        );
        assert_eq!(cleared["status"], json!(200));
        assert!(cleared["body"]["craving"].is_null());

        let bad = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival", "body": { "craving": "gravel" } }),
        );
        assert_eq!(bad["status"], json!(400));
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
    fn crafting_by_recipe_id_and_by_ingredients_both_work() {
        let mut state = hungry_plugin();
        let by_id = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival/craft", "body": { "recipe": "omelette" } }),
        );
        assert_eq!(by_id["status"], json!(200));
        assert_eq!(by_id["body"]["food"], "omelette");
        assert_eq!(by_id["body"]["kind"], "cooked");

        // A different dish, offered as an object of counts this time.
        state.set_levels(Some(40.0), Some(0.0)).expect("override");
        advance_clock(60_000);
        let by_counts = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/survival/craft",
                "body": { "ingredients": { "noodles": 2, "egg": 1, "mushroom": 1 } },
            }),
        );
        assert_eq!(by_counts["status"], json!(200));
        assert_eq!(by_counts["body"]["food"], "ramen");
    }

    #[test]
    fn a_craft_with_no_matching_recipe_is_a_400() {
        let mut state = hungry_plugin();
        let response = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/survival/craft",
                "body": { "ingredients": ["cheese"] },
            }),
        );
        assert_eq!(response["status"], json!(400));
        assert!(response["body"]["error"].as_str().unwrap_or("").contains("cheese"));

        let empty = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival/craft", "body": {} }),
        );
        assert_eq!(empty["status"], json!(400));

        let bad_shape = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival/craft", "body": { "ingredients": 7 } }),
        );
        assert_eq!(bad_shape["status"], json!(400));
    }

    #[test]
    fn a_craft_on_a_full_avatar_is_a_409() {
        let mut state = plugin();
        let response = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival/craft", "body": { "recipe": "pasta" } }),
        );
        assert_eq!(response["status"], json!(409));
        assert_eq!(response["body"]["refusal"], "too_full");
    }

    #[test]
    fn the_catalog_is_served_and_an_unknown_path_is_a_404() {
        let mut state = plugin();
        let foods = handle(&mut state, &json!({ "method": "GET", "path": "/foods" }));
        assert_eq!(foods["status"], json!(200));
        assert!(foods["body"]["count"].as_u64().unwrap_or(0) >= 7);
        assert!(foods["body"]["raw_count"].as_u64().unwrap_or(0) > 0);
        assert!(foods["body"]["crafted_count"].as_u64().unwrap_or(0) > 0);
        // The recipe book rides along, so a client needs one request.
        assert!(!foods["body"]["recipes"].as_array().unwrap().is_empty());

        let recipes = handle(&mut state, &json!({ "method": "GET", "path": "/recipes" }));
        assert_eq!(recipes["status"], json!(200));
        assert!(recipes["body"]["count"].as_u64().unwrap_or(0) >= 6);
        assert!(recipes["body"]["recipes"][0]["ingredients"].is_array());

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

    #[test]
    fn get_inventory_reports_every_slot_and_the_catalog() {
        let mut state = plugin();
        let response = handle(
            &mut state,
            &json!({ "method": "GET", "path": "/inventory" }),
        );
        assert_eq!(response["status"], json!(200));
        let body = &response["body"];
        assert_eq!(body["capacity"], 20);
        assert_eq!(body["used"], 3);
        assert_eq!(body["slots"].as_array().expect("slots").len(), 20);
        assert!(body["catalog"].as_array().expect("catalog").len() >= 9);
        assert!(body["totals"].as_array().expect("totals").len() == 3);
    }

    #[test]
    fn inventory_routes_add_remove_and_use() {
        let mut state = hungry_plugin();
        let added = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/inventory/add",
                "body": { "item": "silver_trout", "count": 2 },
            }),
        );
        assert_eq!(added["status"], json!(200));
        assert_eq!(added["body"]["added"], 2);

        let removed = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/inventory/remove",
                "body": { "id": "silver_trout", "count": 1 },
            }),
        );
        assert_eq!(removed["status"], json!(200));
        assert_eq!(removed["body"]["removed"], 1);

        // `count` defaults to 1 when it is left out.
        let used = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/inventory/use",
                "body": { "item": "silver_trout" },
            }),
        );
        assert_eq!(used["status"], json!(200));
        assert_eq!(used["body"]["item"], "silver_trout");
        assert_eq!(used["body"]["remaining"], 0);
    }

    #[test]
    fn use_accepts_a_slot_index_and_rejects_a_bad_body() {
        let mut state = hungry_plugin();
        handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/inventory/add",
                "body": { "item": "minnow", "count": 2 },
            }),
        );
        // Slot 3 is the first empty one after the starter gear.
        let used = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/inventory/use",
                "body": { "slot": 3 },
            }),
        );
        assert_eq!(used["status"], json!(200));
        assert_eq!(used["body"]["item"], "minnow");

        let empty = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/inventory/use", "body": {} }),
        );
        assert_eq!(empty["status"], json!(400));

        let unknown = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/inventory/add",
                "body": { "item": "diamond" },
            }),
        );
        assert_eq!(unknown["status"], json!(400));
    }

    #[test]
    fn the_fishing_routes_walk_the_whole_session() {
        let mut state = plugin();
        let cast = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/fishing/cast" }),
        );
        assert_eq!(cast["status"], json!(200));
        assert_eq!(cast["body"]["phase"], "waiting");
        assert_eq!(cast["body"]["hook_key"], "Space");

        // A status read while the line is out is honest about the phase.
        let waiting = handle(&mut state, &json!({ "method": "GET", "path": "/fishing" }));
        assert_eq!(waiting["body"]["session"]["phase"], "waiting");
        assert_eq!(waiting["body"]["enabled"], json!(true));

        // Nothing is biting yet, so hooking is refused.
        let early = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/fishing/hook" }),
        );
        assert_eq!(early["status"], json!(409));

        // Skip to the bite the plugin scheduled.
        let session = handle(&mut state, &json!({ "method": "GET", "path": "/fishing" }));
        let _ = session;
        let bite = state.fishing().bite_at_ms();
        crate::host::set_now_ms(bite);
        let biting = handle(&mut state, &json!({ "method": "GET", "path": "/fishing" }));
        assert_eq!(biting["body"]["session"]["phase"], "biting");

        let hooked = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/fishing/hook" }),
        );
        assert_eq!(hooked["status"], json!(200));
        assert_eq!(hooked["body"]["phase"], "reeling");
        assert!(hooked["body"]["reel"]["period_ms"].is_number());

        // Reeling instantly lands the marker at 0.0, outside the spot.
        let reeled = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/fishing/reel" }),
        );
        assert_eq!(reeled["status"], json!(200));
        assert_eq!(reeled["body"]["caught"], json!(false));
        assert_eq!(reeled["body"]["reason"], "missed_the_spot");

        // And there is nothing left to cancel.
        let cancel = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/fishing/cancel" }),
        );
        assert_eq!(cancel["status"], json!(409));
    }

    #[test]
    fn an_unknown_fishing_path_is_a_404() {
        let mut state = plugin();
        let response = handle(
            &mut state,
            &json!({ "method": "GET", "path": "/fishing/nope" }),
        );
        assert_eq!(response["status"], json!(404));
    }
}
