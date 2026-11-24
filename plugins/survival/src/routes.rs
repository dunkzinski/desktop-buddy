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
    ("/scavenge", &["GET"]),
    ("/scavenge/start", &["POST"]),
    ("/scavenge/cancel", &["POST"]),
    ("/scavenge/comment", &["POST"]),
    ("/foods", &["GET"]),
    ("/recipes", &["GET"]),
    ("/store", &["GET"]),
    ("/store/buy", &["POST"]),
    ("/store/sell", &["POST"]),
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

/// Read a trade body: `{"item": "<id>", "count": <number>}`.
///
/// `id` is a synonym for `item` (it is everywhere else in this API) and `count`
/// defaults to `1`. A `count` that is not an integer in 1–[`inventory::MAX_ADD`]
/// is a **400** rather than a silent clamp: a purchase is the one place where
/// quietly changing the number spends the player's coins on something they did
/// not ask for.
fn parse_trade(body: &Value) -> Result<(String, u32), String> {
    let item = body
        .get("item")
        .or_else(|| body.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| "send `item` (an id from GET /store) and optionally `count`".to_string())?;
    let count = match body.get("count") {
        None | Some(Value::Null) => 1,
        Some(value) => {
            let parsed = value
                .as_u64()
                .filter(|count| (1..=u64::from(inventory::MAX_ADD)).contains(count))
                .ok_or_else(|| {
                    format!("`count` must be an integer from 1 to {}", inventory::MAX_ADD)
                })?;
            parsed as u32
        }
    };
    Ok((item.to_string(), count))
}

/// Dispatch one proxied request, then persist any change it made.
///
/// The flush lives **here**, not at the end of every arm: several arms return
/// early (a craft by recipe id, a slot-based use), and an early return that
/// skipped the save would leave a purchase in memory only. One wrapper covers
/// them all, and a `GET` never writes.
pub fn handle(state: &mut Survival, request: &Value) -> Value {
    let response = dispatch(state, request);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET");
    if method.eq_ignore_ascii_case("post") {
        state.flush_state();
    }
    response
}

/// The router itself. Reads never mutate; writes mark the session dirty.
fn dispatch(state: &mut Survival, request: &Value) -> Value {
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
        ("GET", "/foods") => {
            // The recipe book rides along so a client needs one request — with
            // the bag folded in, so `can_cook` is honest there too.
            let mut body = model::food_catalog();
            if let Some(object) = body.as_object_mut() {
                object.insert("recipes".to_string(), state.recipes_json()["recipes"].clone());
            }
            ok(body)
        }
        ("GET", "/recipes") => ok(state.recipes_json()),
        ("GET", "/store") => ok(state.store_json()),
        ("POST", "/store/buy") | ("POST", "/store/sell") => {
            let (item, count) = match parse_trade(&body) {
                Ok(trade) => trade,
                Err(err) => return failed(400, json!({ "ok": false, "error": err })),
            };
            let outcome = if path.ends_with("/buy") {
                state.store_buy(&item, count)
            } else {
                state.store_sell(&item, count)
            };
            match outcome {
                Ok(payload) => ok(payload),
                Err(err) => failed(err.status_code(), err.to_json()),
            }
        }
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
        // Scavenging: the zero-cost fallback. A `GET` advances the session (and
        // may complete it), so the card's poll is all it needs; the two `POST`s
        // start and abandon a run.
        ("GET", "/scavenge") => ok(state.scavenge_status()),
        ("POST", "/scavenge/start") => match state.start_scavenge() {
            Ok(payload) => ok(payload),
            Err(err) => failed(err.status_code(), err.to_json()),
        },
        ("POST", "/scavenge/cancel") => match state.cancel_scavenge() {
            Ok(payload) => ok(payload),
            Err(err) => failed(err.status_code(), err.to_json()),
        },
        // Ambient muttering: the page supplies the window it can see, the guest
        // decides whether this is one of the moments he says something.
        ("POST", "/scavenge/comment") => {
            let window = body.get("window").and_then(Value::as_str);
            match state.scavenge_comment(window) {
                Ok(payload) => ok(payload),
                Err(err) => failed(err.status_code(), err.to_json()),
            }
        }
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

    /// Put `(id, count)` pairs in the bag — cooking draws from the inventory, so
    /// a route test that expects a dish has to shop first.
    fn stock(state: &mut Survival, items: &[(&str, u32)]) {
        for (id, count) in items {
            state
                .inventory_add(id, *count)
                .unwrap_or_else(|err| panic!("stock {id}: {err}"));
        }
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
                "/scavenge",
                "/scavenge/start",
                "/scavenge/cancel",
                "/scavenge/comment",
                "/foods",
                "/recipes",
                "/store",
                "/store/buy",
                "/store/sell"
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
        stock(&mut state, &[("egg", 2), ("cheese", 1)]);
        let by_id = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival/craft", "body": { "recipe": "omelette" } }),
        );
        assert_eq!(by_id["status"], json!(200));
        assert_eq!(by_id["body"]["food"], "omelette");
        assert_eq!(by_id["body"]["kind"], "cooked");
        // The pantry paid for it.
        assert_eq!(by_id["body"]["consumed"].as_array().expect("consumed").len(), 2);

        // A different dish, offered as an object of counts this time.
        state.set_levels(Some(40.0), Some(0.0)).expect("override");
        stock(&mut state, &[("noodles", 2), ("egg", 1), ("mushroom", 1)]);
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
    fn cooking_without_the_ingredients_is_a_409_naming_what_is_short() {
        let mut state = hungry_plugin();
        stock(&mut state, &[("egg", 1)]);
        let response = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival/craft", "body": { "recipe": "omelette" } }),
        );
        assert_eq!(response["status"], json!(409));
        assert_eq!(response["body"]["refusal"], "missing_ingredients");
        assert_eq!(response["body"]["missing"][0]["item"], "egg");
        assert_eq!(response["body"]["missing"][0]["short"], json!(1));
        assert_eq!(response["body"]["missing"][1]["item"], "cheese");
        // Nothing was spent.
        assert_eq!(state.inventory().count("egg"), 1);
    }

    #[test]
    fn the_recipe_book_reports_what_the_bag_can_cook() {
        let mut state = hungry_plugin();
        let before = handle(&mut state, &json!({ "method": "GET", "path": "/recipes" }));
        assert_eq!(before["status"], json!(200));
        assert!(before["body"]["recipes"]
            .as_array()
            .expect("recipes")
            .iter()
            .all(|recipe| recipe["can_cook"] == json!(false)));

        stock(&mut state, &[("egg", 2), ("cheese", 1)]);
        let after = handle(&mut state, &json!({ "method": "GET", "path": "/recipes" }));
        let omelette = after["body"]["recipes"]
            .as_array()
            .expect("recipes")
            .iter()
            .find(|recipe| recipe["id"] == "omelette")
            .expect("omelette");
        assert_eq!(omelette["can_cook"], json!(true));
        assert_eq!(omelette["ingredients"][0]["held"], json!(2));

        // `GET /foods` carries the same enriched book, not the static one.
        let foods = handle(&mut state, &json!({ "method": "GET", "path": "/foods" }));
        let omelette = foods["body"]["recipes"]
            .as_array()
            .expect("recipes")
            .iter()
            .find(|recipe| recipe["id"] == "omelette")
            .expect("omelette");
        assert_eq!(omelette["can_cook"], json!(true));
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
        // The pantry is stocked, so the *only* reason left to refuse is the full
        // belly — which is the point of this test.
        stock(&mut state, &[("noodles", 1), ("tomato", 2), ("cheese", 1)]);
        let response = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/survival/craft", "body": { "recipe": "pasta" } }),
        );
        assert_eq!(response["status"], json!(409));
        assert_eq!(response["body"]["refusal"], "too_full");
        // A refused cook spends nothing.
        assert_eq!(state.inventory().count("noodles"), 1);
        assert_eq!(state.inventory().count("cheese"), 1);
        assert_eq!(state.inventory().count("tomato"), 2);
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

    /* -------------------------------------------------------------- *
     * Store & economy
     * -------------------------------------------------------------- */

    /// A four-slot bag with every slot taken — the only honest way to make a
    /// purchase fail on capacity.
    fn full_bag() -> Survival {
        let config = SurvivalConfig {
            inventory: crate::inventory::InventoryConfig { slots: 4 },
            ..SurvivalConfig::default()
        };
        let mut state = Survival::new(config);
        // The starter gear claims three slots; a pair of boots claims the last.
        state
            .inventory_add("old_boot", 2)
            .expect("fill the last slot");
        assert!(state.inventory().is_full());
        take_recorded();
        state
    }

    #[test]
    fn get_store_serves_the_catalog_wallet_and_holdings() {
        let mut state = plugin();
        let response = handle(&mut state, &json!({ "method": "GET", "path": "/store" }));
        assert_eq!(response["status"], json!(200));
        let body = &response["body"];
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["coins"], json!(100), "a fresh session starts with 100");
        assert_eq!(body["starting_coins"], json!(100));
        assert_eq!(
            body["count"],
            json!(crate::store::default_catalog().len()),
            "count matches the served catalog"
        );

        let catalog = body["catalog"].as_array().expect("catalog");
        assert_eq!(catalog.len(), body["count"].as_u64().unwrap() as usize);
        let worm = catalog
            .iter()
            .find(|entry| entry["id"] == "worm_bait")
            .expect("worm bait is stocked");
        assert_eq!(worm["buy_price"], json!(4));
        assert_eq!(worm["sell_price"], json!(1));
        assert_eq!(worm["category"], json!("bait"));
        assert_eq!(worm["owned"], json!(5), "the starter bag holds five");
        assert_eq!(body["categories"].as_array().unwrap().len(), 4);
        assert!(body["inventory"]["free"].is_number());
    }

    #[test]
    fn buying_deducts_the_tab_and_fills_the_bag() {
        let mut state = plugin();
        let response = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/store/buy",
                "body": { "item": "worm_bait", "count": 3 },
            }),
        );
        assert_eq!(response["status"], json!(200));
        let body = &response["body"];
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["action"], "buy");
        assert_eq!(body["item"], "worm_bait");
        assert_eq!(body["count"], json!(3));
        assert_eq!(body["unit_price"], json!(4));
        assert_eq!(body["total"], json!(12), "3 x 4 coins");
        assert_eq!(body["coins"], json!(88), "100 - 12");
        assert!(body["inventory"]["free"].is_number());

        assert_eq!(state.coins(), 88);
        assert_eq!(state.inventory().count("worm_bait"), 8, "5 starter + 3 bought");
        let events = take_recorded().events;
        assert!(
            events
                .iter()
                .any(|(name, _)| name == crate::plugin::EVENT_STORE),
            "a purchase publishes store-transaction"
        );
    }

    #[test]
    fn an_unaffordable_purchase_is_a_409_and_moves_nothing() {
        let mut state = plugin();
        // Two rods at 75 is 150 coins against a purse of 100.
        let response = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/store/buy",
                "body": { "item": "fishing_rod", "count": 2 },
            }),
        );
        assert_eq!(response["status"], json!(409));
        let body = &response["body"];
        assert_eq!(body["ok"], json!(false));
        assert_eq!(body["refusal"], "insufficient_coins");
        assert_eq!(body["required"], json!(150));
        assert_eq!(body["shortfall"], json!(50));
        assert_eq!(state.coins(), 100, "a refused purchase is free");
        assert_eq!(state.inventory().count("fishing_rod"), 1, "one starter rod");
    }

    #[test]
    fn buying_into_a_full_bag_is_a_409_and_costs_nothing() {
        let mut state = full_bag();
        // River weed is not in the starter bag, so it needs a *slot* — and every
        // slot is taken. (Bait would top up the existing stack instead, which is
        // exactly the difference `room_for` exists to see.)
        let response = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/store/buy",
                "body": { "item": "river_weed", "count": 1 },
            }),
        );
        assert_eq!(response["status"], json!(409));
        assert_eq!(response["body"]["refusal"], "inventory_full");
        assert_eq!(response["body"]["room"], json!(0));
        assert_eq!(state.coins(), 100, "coins are only spent on a real purchase");
        assert_eq!(state.inventory().count("river_weed"), 0, "nothing landed");
    }

    #[test]
    fn selling_awards_coins_and_removes_the_goods() {
        let mut state = plugin();
        let response = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/store/sell",
                "body": { "item": "worm_bait", "count": 3 },
            }),
        );
        assert_eq!(response["status"], json!(200));
        let body = &response["body"];
        assert_eq!(body["action"], "sell");
        assert_eq!(body["count"], json!(3));
        assert_eq!(body["unit_price"], json!(1));
        assert_eq!(body["total"], json!(3));
        assert_eq!(body["coins"], json!(103), "100 + 3");
        assert_eq!(state.coins(), 103);
        assert_eq!(state.inventory().count("worm_bait"), 2);
    }

    #[test]
    fn overselling_or_selling_something_that_is_not_an_item_is_refused() {
        let mut state = plugin();
        let too_many = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/store/sell",
                "body": { "item": "worm_bait", "count": 9 },
            }),
        );
        assert_eq!(too_many["status"], json!(409));
        assert_eq!(too_many["body"]["refusal"], "not_enough_items");
        assert_eq!(too_many["body"]["held"], json!(5));
        assert_eq!(state.coins(), 100, "no partial sale, no partial payment");

        // Not an inventory id at all: nothing to price, nothing to take.
        let unknown = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/store/sell",
                "body": { "item": "diamond", "count": 1 },
            }),
        );
        assert_eq!(unknown["status"], json!(400));

        let unbuyable = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/store/buy",
                "body": { "item": "diamond", "count": 1 },
            }),
        );
        assert_eq!(unbuyable["status"], json!(400));
        assert!(unbuyable["body"]["error"]
            .as_str()
            .unwrap_or("")
            .contains("diamond"));
    }

    #[test]
    fn the_counter_buys_a_caught_fish_the_shelf_never_stocks() {
        // The shelf still sells the same thirteen lines…
        let mut state = plugin();
        let store = handle(&mut state, &json!({ "method": "GET", "path": "/store" }));
        assert!(
            store["body"]["catalog"]
                .as_array()
                .expect("catalog")
                .iter()
                .all(|entry| entry["category"] != "fish"),
            "raw fish must not clutter the shelves"
        );

        // …but the counter takes the catch, at the rarity-derived price.
        state.inventory_add("silver_trout", 3).expect("stock");
        take_recorded();
        let sold = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/store/sell",
                "body": { "item": "silver_trout", "count": 2 },
            }),
        );
        assert_eq!(sold["status"], json!(200));
        let body = &sold["body"];
        assert_eq!(body["action"], "sell");
        assert_eq!(body["item"], "silver_trout");
        assert_eq!(body["label"], "Silver trout");
        assert_eq!(body["unit_price"], json!(6), "2 x rarity 3");
        assert_eq!(body["total"], json!(12));
        assert_eq!(body["coins"], json!(112));
        assert_eq!(state.coins(), 112);
        assert_eq!(state.inventory().count("silver_trout"), 1, "one left");

        let recorded = take_recorded();
        assert!(
            recorded
                .events
                .iter()
                .any(|(name, payload)| name == crate::plugin::EVENT_STORE
                    && payload["action"] == "sell"
                    && payload["item"] == "silver_trout"),
            "a fish sale publishes store-transaction"
        );
        assert_eq!(recorded.states.len(), 1, "the sale is flushed to disk");
    }

    #[test]
    fn a_post_writes_the_session_and_a_get_never_does() {
        let mut state = hungry_plugin();
        take_recorded();

        let response = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/store/buy",
                "body": { "item": "egg", "count": 2 },
            }),
        );
        assert_eq!(response["status"], json!(200));
        let recorded = take_recorded();
        assert_eq!(
            recorded.states.len(),
            1,
            "a mutation is on disk before the reply lands"
        );
        let saved: Value = serde_json::from_slice(&recorded.states[0]).expect("json");
        assert_eq!(saved["stats"]["coins"], json!(94), "2 eggs cost 6");
        assert!(saved["inventory"]["stacks"]
            .as_array()
            .expect("stacks")
            .iter()
            .any(|row| row["id"] == "egg" && row["count"] == 2));

        // A read is not a change.
        handle(&mut state, &json!({ "method": "GET", "path": "/store" }));
        assert!(take_recorded().states.is_empty(), "GET must not write");

        // A refused trade is not a change either.
        let refused = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/store/buy",
                "body": { "item": "fishing_rod", "count": 5 },
            }),
        );
        assert_eq!(refused["status"], json!(409));
        assert!(
            take_recorded().states.is_empty(),
            "a refused purchase must not touch the save"
        );
    }

    #[test]
    fn a_craft_saves_even_though_its_arm_returns_early() {
        // The recipe arm answers with `return`, which is exactly why the flush
        // is a wrapper around the dispatcher and not a line at the end of it.
        let mut state = hungry_plugin();
        stock(&mut state, &[("egg", 2), ("cheese", 1)]);
        take_recorded();

        let response = handle(
            &mut state,
            &json!({
                "method": "POST",
                "path": "/survival/craft",
                "body": { "recipe": "omelette" },
            }),
        );
        assert_eq!(response["status"], json!(200));
        let recorded = take_recorded();
        assert_eq!(recorded.states.len(), 1, "the early return still flushed");
        let saved: Value = serde_json::from_slice(&recorded.states[0]).expect("json");
        assert!(
            saved["inventory"]["stacks"]
                .as_array()
                .expect("stacks")
                .iter()
                .all(|row| row["id"] != "egg" && row["id"] != "cheese"),
            "the saved bag must be the one after the pantry paid: {saved}"
        );
        assert!(
            saved["stats"]["hunger"].as_f64().unwrap() > 40.0,
            "the saved bars must be the ones after the meal"
        );
    }

    #[test]
    fn the_scavenge_routes_start_observe_and_cancel_a_run() {
        let mut state = hungry_plugin();

        let started = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/scavenge/start" }),
        );
        assert_eq!(started["status"], json!(200));
        assert_eq!(started["body"]["phase"], "rummaging");
        assert!(started["body"]["ends_ms"].as_u64().unwrap() > started["body"]["started_ms"].as_u64().unwrap());

        // The window's own read: same session, and it is what the card polls.
        let status = handle(&mut state, &json!({ "method": "GET", "path": "/scavenge" }));
        assert_eq!(status["status"], json!(200));
        assert_eq!(status["body"]["session"]["phase"], "rummaging");
        assert_eq!(status["body"]["enabled"], json!(true));
        assert!(status["body"]["session"]["remaining_secs"].as_u64().unwrap() <= 30);

        // A second start while one is out is a refusal, not a restart.
        let again = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/scavenge/start" }),
        );
        assert_eq!(again["status"], json!(409));

        let cancelled = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/scavenge/cancel" }),
        );
        assert_eq!(cancelled["status"], json!(200));
        assert_eq!(cancelled["body"]["reason"], "cancelled");
        assert_eq!(
            handle(&mut state, &json!({ "method": "POST", "path": "/scavenge/cancel" }))["status"],
            json!(409)
        );
    }

    #[test]
    fn the_comment_route_mutters_only_while_out() {
        let mut state = hungry_plugin();
        let idle = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/scavenge/comment", "body": { "window": "Hermes" } }),
        );
        assert_eq!(idle["status"], json!(409), "nothing to comment on while idle");

        handle(&mut state, &json!({ "method": "POST", "path": "/scavenge/start" }));
        // The default chance is 300 per mille, so this either speaks or declines —
        // both are 200s. What must hold is the shape.
        let response = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/scavenge/comment", "body": { "window": "Hermes" } }),
        );
        assert_eq!(response["status"], json!(200));
        assert!(response["body"]["spoke"].is_boolean());
        assert!(response["body"]["session"]["phase"].is_string());
        // A body-less request is legal too: he mutters something generic.
        let generic = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/scavenge/comment" }),
        );
        assert_eq!(generic["status"], json!(200));
        assert!(generic["body"]["window"].is_null());
    }

    #[test]
    fn a_get_scavenge_finishes_the_run_and_persists_it() {
        // The card's poll can be the caller that first sees the deadline, and a
        // GET never reaches the dispatcher's flush — so completion saves itself.
        let mut state = hungry_plugin();
        let started = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/scavenge/start" }),
        );
        let ends = started["body"]["ends_ms"].as_u64().expect("ends_ms");
        let coins_before = state.coins();
        advance_clock(ends - crate::host::now_ms());
        take_recorded();

        let status = handle(&mut state, &json!({ "method": "GET", "path": "/scavenge" }));
        assert_eq!(status["status"], json!(200));
        assert_eq!(status["body"]["session"]["phase"], "idle");
        assert!(state.coins() > coins_before, "the run paid out on the poll");
        let recorded = take_recorded();
        assert!(
            recorded
                .events
                .iter()
                .any(|(name, _)| name == crate::plugin::EVENT_SCAVENGING_FINISHED),
            "the poll published the findings"
        );
        assert!(!recorded.states.is_empty(), "and wrote them to the save");
    }

    #[test]
    fn a_trade_count_must_be_a_real_positive_integer() {
        for bad in [json!(0), json!(100), json!("three"), json!(-1), json!(1.5)] {
            let mut state = plugin();
            let response = handle(
                &mut state,
                &json!({
                    "method": "POST",
                    "path": "/store/buy",
                    "body": { "item": "worm_bait", "count": bad },
                }),
            );
            assert_eq!(response["status"], json!(400), "count {bad} must be refused");
        }

        // A missing count is one unit, not zero.
        let mut state = plugin();
        let response = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/store/sell", "body": { "item": "worm_bait" } }),
        );
        assert_eq!(response["status"], json!(200));
        assert_eq!(response["body"]["count"], json!(1));
        assert_eq!(state.coins(), 101);
    }
}
