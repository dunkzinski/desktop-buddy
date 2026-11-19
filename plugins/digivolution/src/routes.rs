//! The plugin's HTTP surface, declared as data.
//!
//! The host asks for [`ROUTES`] once (as JSON) and builds axum routes from it;
//! every request then arrives at [`handle`] as `{method, path, body}`. Nothing
//! in the app names `/digimon/status` or `/digivolve` — they appear the moment
//! `digivolution.wasm` is in `plugins/`.
//!
//! The two endpoints the feature is specified around:
//!
//! * `GET  /digimon/status` — stage, XP, bond, attribute, the live rig's
//!   measurements and the branch table.
//! * `POST /digivolve` — generate the next stage, write
//!   `models/<slug>/model.json`, hot-swap the avatar (`POST /model`'s code
//!   path) and play the `digivolve` clip.

use serde_json::{json, Value};

use crate::evolution::Attribute;
use crate::generator::Archetype;
use crate::plugin::Digimon;

/// The single source of truth for this plugin's endpoints.
pub const ROUTES: &[(&str, &[&str])] = &[
    ("/digimon/status", &["GET"]),
    ("/digivolve", &["POST"]),
    ("/dedigivolve", &["POST"]),
    ("/digimon/xp", &["POST"]),
    ("/digimon/reset", &["POST"]),
];

/// `[{"path": "/digimon/status", "methods": ["GET"]}, …]`
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

/// Dispatch one proxied request, then persist any change it made.
pub fn handle(state: &mut Digimon, request: &Value) -> Value {
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

fn dispatch(state: &mut Digimon, request: &Value) -> Value {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("GET")
        .to_ascii_uppercase();
    let raw_path = request.get("path").and_then(Value::as_str).unwrap_or("/");
    let path = raw_path.split('?').next().unwrap_or("/");
    let body = request.get("body").cloned().unwrap_or(Value::Null);

    match (method.as_str(), path) {
        ("GET", "/digimon/status") => ok(state.status_json()),
        ("POST", "/digivolve") => {
            let branch = body.get("branch").and_then(Value::as_u64).map(|v| v as usize);
            let force = body
                .get("force")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            match state.digivolve(branch, force) {
                Ok(outcome) => {
                    let mut payload = json!({
                        "ok": true,
                        "from": outcome.from.as_str(),
                        "stage": outcome.to.as_str(),
                        "slug": outcome.slug,
                        "label": outcome.label,
                        "attribute": outcome.attribute.as_str(),
                        // How the lineage mix shifted: the mixed rig's weights,
                        // percentages and blended traits.
                        "attribute_blend": crate::plugin::blend_json(
                            outcome.blend,
                            &state.state().lineage(),
                        ),
                        "blend_summary": outcome.blend_summary,
                        "animation": outcome.animation,
                        "bones": outcome.bones,
                        "parts": outcome.parts,
                        "min_world_y": outcome.min_world_y,
                        "model_path": format!("models/{}/model.json", outcome.slug),
                    });
                    // Fold the post-transition status in: one request is enough
                    // for a client to repaint everything.
                    payload["status"] = state.status_json();
                    ok(payload)
                }
                Err(err) => {
                    state.record_error(err.clone());
                    let status = if err.starts_with("not ready") {
                        409
                    } else if err.contains("final stage") {
                        409
                    } else {
                        400
                    };
                    failed(status, json!({ "ok": false, "error": err }))
                }
            }
        }
        ("POST", "/dedigivolve") => {
            let steps = body
                .get("steps")
                .and_then(Value::as_u64)
                .unwrap_or(1) as usize;
            if steps == 0 {
                return failed(
                    400,
                    json!({ "ok": false, "error": "`steps` must be at least 1" }),
                );
            }
            match state.dedigivolve(steps) {
                Ok(outcome) => {
                    let mut payload = json!({
                        "ok": true,
                        "direction": "out",
                        "steps": steps,
                        "from": outcome.from.as_str(),
                        "stage": outcome.to.as_str(),
                        "slug": outcome.slug,
                        "label": outcome.label,
                        "attribute": outcome.attribute.as_str(),
                        // The mix the creature returns to — the truncated
                        // lineage's weights, not the mix it had a moment ago.
                        "attribute_blend": crate::plugin::blend_json(
                            outcome.blend,
                            &state.state().lineage(),
                        ),
                        "blend_summary": outcome.blend_summary,
                        "animation": outcome.animation,
                        "bones": outcome.bones,
                        "parts": outcome.parts,
                        "min_world_y": outcome.min_world_y,
                        "model_path": format!("models/{}/model.json", outcome.slug),
                    });
                    payload["status"] = state.status_json();
                    ok(payload)
                }
                Err(err) => {
                    state.record_error(err.clone());
                    let status = if err.contains("no earlier stage") { 409 } else { 400 };
                    failed(status, json!({ "ok": false, "error": err }))
                }
            }
        }
        ("POST", "/digimon/xp") => {
            let xp = body.get("xp").and_then(Value::as_u64).unwrap_or(0);
            let bond = body
                .get("bond")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .clamp(-1.0, 1.0);
            if xp == 0 && bond == 0.0 {
                return failed(
                    400,
                    json!({ "ok": false, "error": "send `xp` and/or `bond`" }),
                );
            }
            let mut payload = state.award(xp, bond);
            payload["status"] = state.status_json();
            ok(payload)
        }
        ("POST", "/digimon/reset") => {
            let seed = body.get("seed").and_then(Value::as_u64);
            let archetype = body
                .get("archetype")
                .and_then(Value::as_str)
                .and_then(Archetype::parse);
            if body.get("archetype").is_some() && archetype.is_none() {
                return failed(
                    400,
                    json!({
                        "ok": false,
                        "error": format!(
                            "`archetype` must be one of {}",
                            Archetype::ALL
                                .iter()
                                .map(|archetype| archetype.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    }),
                );
            }
            let attribute = body
                .get("attribute")
                .and_then(Value::as_str)
                .and_then(Attribute::parse);
            if body.get("attribute").is_some() && attribute.is_none() {
                return failed(
                    400,
                    json!({ "ok": false, "error": "`attribute` must be Vaccine, Data or Virus" }),
                );
            }
            match state.reset(seed, archetype, attribute) {
                Ok(status) => {
                    // A fresh creature is a one-hot blend of whichever attribute
                    // was asked for; report it at the top level like the other
                    // transitions do, as well as inside `status`.
                    let blend =
                        crate::plugin::blend_json(state.state().blend(), &state.state().lineage());
                    ok(json!({ "ok": true, "attribute_blend": blend, "status": status }))
                }
                Err(err) => {
                    state.record_error(err.clone());
                    failed(500, json!({ "ok": false, "error": err }))
                }
            }
        }
        _ => failed(
            404,
            json!({ "ok": false, "error": format!("{method} {path} is not a digivolution route") }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evolution::Digivolution;
    use crate::generator::Tier;

    fn plugin() -> Digimon {
        let mut state = Digimon::new(Digivolution::new(31, Archetype::Holy, Attribute::Vaccine));
        state.ensure_installed().expect("install");
        crate::host::take_recorded();
        state
    }

    #[test]
    fn the_route_table_is_the_documented_one() {
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
                "/digimon/status",
                "/digivolve",
                "/dedigivolve",
                "/digimon/xp",
                "/digimon/reset"
            ]
        );
    }

    #[test]
    fn get_status_answers_the_documented_shape() {
        let mut state = plugin();
        let response = handle(
            &mut state,
            &json!({ "method": "GET", "path": "/digimon/status" }),
        );
        assert_eq!(response["status"], json!(200));
        let body = &response["body"];
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["stage"], json!("Fresh"));
        assert_eq!(body["attribute"], json!("Vaccine"));
        assert!(body["branches"].as_array().unwrap().len() == 3);
        assert!(body["rig"]["bones"].as_u64().unwrap() > 0);

        // The lineage mix, in the shape the card reads.
        let blend = &body["attribute_blend"];
        assert_eq!(blend["dominant"], json!("Vaccine"));
        assert_eq!(blend["percent"]["Vaccine"], json!(100));
        assert_eq!(blend["summary"], json!("100% Vaccine"));
        assert_eq!(blend["pure"], json!(true));
        assert_eq!(blend["lineage"], json!(["Vaccine"]));
        assert_eq!(body["rig"]["parts_budget"], json!(100), "Fresh is validated at 100");

        // Every branch previews the mix it would produce.
        for branch in body["branches"].as_array().unwrap() {
            assert!(branch["blend"]["weights"].is_array(), "{branch}");
            assert!(
                branch["blend_summary"].as_str().map(|s| s.contains('%')).unwrap_or(false),
                "{branch}"
            );
            assert!(!branch["blended_flavour"].as_str().unwrap_or("").is_empty());
        }
    }

    #[test]
    fn digivolve_refuses_without_xp_and_accepts_with_force() {
        let mut state = plugin();
        let refused = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/digivolve", "body": {} }),
        );
        assert_eq!(refused["status"], json!(409));
        assert!(refused["body"]["error"]
            .as_str()
            .unwrap_or("")
            .contains("not ready"));

        let forced = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/digivolve", "body": { "force": true } }),
        );
        assert_eq!(forced["status"], json!(200));
        let body = &forced["body"];
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["from"], json!("Fresh"));
        assert_eq!(body["stage"], json!("In-Training"));
        assert_eq!(body["animation"], json!("digivolve"));
        assert_eq!(
            body["model_path"],
            json!(format!("models/{}/model.json", body["slug"].as_str().unwrap()))
        );
        assert_eq!(body["status"]["stage"], json!("In-Training"));

        // The reply names the mix the new body was generated from: the egg's
        // attribute plus the one this branch just chose.
        assert_eq!(body["blend_summary"], body["attribute_blend"]["summary"]);
        assert_eq!(
            body["attribute_blend"]["lineage"].as_array().unwrap().len(),
            2,
            "the lineage grew by the chosen attribute"
        );
        assert_eq!(body["attribute_blend"]["pure"], json!(true), "one step of three attributes");
    }

    #[test]
    fn digivolve_rejects_a_branch_out_of_range() {
        let mut state = plugin();
        let response = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/digivolve", "body": { "branch": 9, "force": true } }),
        );
        assert_eq!(response["status"], json!(400));
        assert!(response["body"]["error"]
            .as_str()
            .unwrap_or("")
            .contains("branch 9"));
    }

    #[test]
    fn dedigivolve_rewinds_and_refuses_at_fresh() {
        let mut state = plugin();
        let fresh = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/dedigivolve", "body": {} }),
        );
        assert_eq!(fresh["status"], json!(409), "Fresh has no parent");
        assert!(fresh["body"]["error"]
            .as_str()
            .unwrap_or("")
            .contains("no earlier stage"));

        let evolved = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/digivolve", "body": { "force": true } }),
        );
        let slug = evolved["body"]["slug"].as_str().unwrap().to_string();

        let back = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/dedigivolve", "body": { "steps": 1 } }),
        );
        assert_eq!(back["status"], json!(200));
        let body = &back["body"];
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["direction"], json!("out"));
        assert_eq!(body["from"], json!("In-Training"));
        assert_eq!(body["stage"], json!("Fresh"));
        assert_ne!(body["slug"].as_str().unwrap(), slug.as_str());
        assert_eq!(body["model_path"], json!(format!("models/{}/model.json", body["slug"].as_str().unwrap())));
        assert_eq!(body["status"]["stage"], json!("Fresh"));
        assert_eq!(body["status"]["can_dedigivolve"], json!(false));
        // A rewind puts the earlier mix back on the creature, not just the stage.
        assert_eq!(body["status"]["attribute_blend"]["pure"], json!(true));
        assert_eq!(body["status"]["attribute_blend"]["lineage"], json!(["Vaccine"]));

        let zero = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/dedigivolve", "body": { "steps": 0 } }),
        );
        assert_eq!(zero["status"], json!(400));
    }

    #[test]
    fn xp_and_reset_work_and_bad_bodies_are_400s() {
        let mut state = plugin();
        let xp = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/digimon/xp", "body": { "xp": 40, "bond": 0.2 } }),
        );
        assert_eq!(xp["status"], json!(200));
        assert_eq!(xp["body"]["can_evolve"], json!(true));

        let empty = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/digimon/xp", "body": {} }),
        );
        assert_eq!(empty["status"], json!(400));

        let bad_archetype = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/digimon/reset", "body": { "archetype": "toaster" } }),
        );
        assert_eq!(bad_archetype["status"], json!(400));

        let reset = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/digimon/reset", "body": { "archetype": "cyborg", "seed": 5 } }),
        );
        assert_eq!(reset["status"], json!(200));
        assert_eq!(reset["body"]["status"]["stage"], json!("Fresh"));
        assert_eq!(
            reset["body"]["attribute_blend"]["summary"],
            json!("100% Vaccine"),
            "a reset reports the new creature's one-hot mix (the inherited attribute)"
        );
        assert_eq!(reset["body"]["status"]["creature"]["archetype"], json!("cyborg"));
    }

    #[test]
    fn the_last_stage_cannot_evolve_again() {
        let mut state = plugin();
        for _ in 0..5 {
            handle(
                &mut state,
                &json!({ "method": "POST", "path": "/digivolve", "body": { "force": true } }),
            );
        }
        assert_eq!(state.state().tier, Tier::Mega);
        let response = handle(
            &mut state,
            &json!({ "method": "POST", "path": "/digivolve", "body": { "force": true } }),
        );
        assert_eq!(response["status"], json!(409));
    }

    #[test]
    fn an_unknown_path_is_a_404() {
        let mut state = plugin();
        let response = handle(
            &mut state,
            &json!({ "method": "GET", "path": "/digimon/nope" }),
        );
        assert_eq!(response["status"], json!(404));
    }
}
