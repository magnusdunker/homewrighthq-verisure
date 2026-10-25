//! Mapping tests on answers shaped like Verisure's, and the plugin against a
//! fake Verisure with HomewrightHQ played over a pipe.

mod host;

use homewright_verisure::api::{ApiError, Client, Urls};
use homewright_verisure::model;
use homewright_verisure::Verisure;
use serde_json::{json, Value};
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn fixture(name: &str) -> Value {
    let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn overview() -> serde_json::Map<String, Value> {
    model::merge(fixture("overview.json").as_array().unwrap())
}

#[test]
fn installations_and_overview() {
    let i = model::installations(fixture("installations.json").as_array().unwrap());
    assert_eq!(i.len(), 2);
    assert_eq!(i[0].alias, "Hjemme");
    assert_eq!(i[0].pin_length, Some(6));
    // no alias: the street
    assert_eq!(i[1].alias, "Sommerhusvej 4");

    let o = overview();
    assert_eq!(model::alarm_state(&o), Some("armed_home"));
    assert_eq!(model::alarm_changed_by(&o).as_deref(), Some("Mette"));
    let c = model::climates(&o);
    assert_eq!(c[0].name, "Stue");
    assert_eq!(c[0].temperature, Some(21.4));
    assert_eq!(c[0].humidity, Some(48.0));
    assert_eq!(c[1].humidity, None);
    let d = model::door_windows(&o);
    assert_eq!(d[0].open, Some(false));
    assert_eq!(d[1].open, Some(true));
    let l = &model::smart_locks(&o)[0];
    assert_eq!((l.locked, l.door_open, l.method.as_deref()), (Some(true), Some(false), Some("AUTO")));
    assert_eq!(model::smart_plugs(&o)[0].on, Some(false));
}

fn has_cookie(name: &'static str) -> impl Fn(&Request) -> bool + Send + Sync {
    move |r: &Request| r.headers.get("cookie").and_then(|c| c.to_str().ok()).is_some_and(|c| c.split("; ").any(|p| p.starts_with(&format!("{name}="))))
}

/// The answer to each operation in a GraphQL request.
fn graphql_answer(req: &Request) -> ResponseTemplate {
    let ops: Vec<Value> = serde_json::from_slice(&req.body).unwrap();
    let ov = fixture("overview.json");
    let answers: Vec<Value> = ops
        .iter()
        .map(|op| match op["operationName"].as_str().unwrap() {
            "fetchAllInstallations" => fixture("installations.json")[0].clone(),
            "ArmState" => ov[0].clone(),
            "Climate" => ov[1].clone(),
            "DoorWindow" => ov[2].clone(),
            "SmartLock" => ov[3].clone(),
            "SmartPlug" => ov[4].clone(),
            "armAway" | "armHome" | "disarm" | "DoorLock" | "DoorUnlock" if op["variables"]["code"] == "000000" || op["variables"]["input"]["code"] == "000000" => {
                json!({ "data": null, "errors": [{ "message": "Request Failed", "data": { "errorCode": "VAL_00819", "errorMessage": "Invalid code" } }] })
            }
            name => json!({ "data": { name: "transaction-1" } }),
        })
        .collect();
    ResponseTemplate::new(200).set_body_json(answers)
}

async fn cloud(server: &MockServer) {
    // the password is right, and two-step login is on
    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .and(|r: &Request| r.headers.get("authorization").is_some_and(|h| h == "Basic YUBiLmRrOnNlY3JldA=="))
        .and(has_cookie("vs-trust"))
        .respond_with(ResponseTemplate::new(200).insert_header("set-cookie", "vs-access=A1; Path=/; HttpOnly").append_header("set-cookie", "vs-refresh=R1; Path=/").set_body_json(json!({})))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/auth/login"))
        .and(|r: &Request| r.headers.get("authorization").is_some_and(|h| h == "Basic YUBiLmRrOnNlY3JldA=="))
        .respond_with(ResponseTemplate::new(200).insert_header("set-cookie", "vs-stepup=S1; Path=/").set_body_json(fixture("login_stepup.json")))
        .mount(server)
        .await;
    Mock::given(method("POST")).and(path("/auth/login")).respond_with(ResponseTemplate::new(401)).mount(server).await;
    Mock::given(method("POST")).and(path("/auth/mfa")).and(query_param("type", "phone")).and(has_cookie("vs-stepup")).respond_with(ResponseTemplate::new(200)).mount(server).await;
    Mock::given(method("POST"))
        .and(path("/auth/mfa/validate"))
        .and(body_json(json!({ "token": "123456" })))
        .and(has_cookie("vs-stepup"))
        .respond_with(ResponseTemplate::new(200).insert_header("set-cookie", "vs-access=A1; Path=/").append_header("set-cookie", "vs-refresh=R1; Path=/"))
        .mount(server)
        .await;
    Mock::given(method("POST")).and(path("/auth/mfa/validate")).respond_with(ResponseTemplate::new(400)).mount(server).await;
    Mock::given(method("POST")).and(path("/auth/trust")).respond_with(ResponseTemplate::new(200).insert_header("set-cookie", "vs-trust=T1; Path=/; Max-Age=31536000")).mount(server).await;
    Mock::given(method("POST")).and(path("/graphql")).and(has_cookie("vs-access")).respond_with(graphql_answer).mount(server).await;
    Mock::given(method("POST")).and(path("/graphql")).respond_with(ResponseTemplate::new(401)).mount(server).await;
}

fn flow(data: Value) -> Value {
    json!({ "flow_id": "f1", "source": "user", "data": data })
}

#[tokio::test(flavor = "multi_thread")]
async fn flow_with_code_setup_and_commands() {
    let server = MockServer::start().await;
    cloud(&server).await;
    let h = host::FakeHost::start(Verisure::default());
    let base = json!({ "base_url": server.uri() });

    let r = h.call("config_flow", json!({ "flow": flow(base.clone()), "step": "user", "input": null })).await.unwrap();
    let keys: Vec<&String> = r["result"]["schema"]["properties"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["email", "password", "mfa_type"]);

    let r = h.call("config_flow", json!({ "flow": flow(base.clone()), "step": "user", "input": { "email": "a@b.dk", "password": "wrong" } })).await.unwrap();
    assert_eq!(r["result"]["errors"]["base"], "invalid_auth");

    let r = h.call("config_flow", json!({ "flow": flow(base.clone()), "step": "user", "input": { "email": " a@b.dk ", "password": "secret", "mfa_type": "phone" } })).await.unwrap();
    assert_eq!(r["result"]["step_id"], "mfa");
    let d = r["data"].clone();
    assert_eq!(d["cookies"]["vs-stepup"], "S1");

    let r = h.call("config_flow", json!({ "flow": flow(d.clone()), "step": "mfa", "input": { "code": "999999" } })).await.unwrap();
    assert_eq!(r["result"]["errors"]["base"], "invalid_code");
    let r = h.call("config_flow", json!({ "flow": flow(d.clone()), "step": "mfa", "input": { "code": "123 456" } })).await.unwrap();
    assert_eq!(r["result"]["step_id"], "installation");
    assert_eq!(r["result"]["schema"]["properties"]["giid"]["oneOf"][1], json!({ "const": "123456789111", "title": "Sommerhusvej 4" }));
    let d = r["data"].clone();
    assert_eq!(d["cookies"]["vs-trust"], "T1");

    let r = h.call("config_flow", json!({ "flow": flow(d.clone()), "step": "installation", "input": { "giid": "123456789000" } })).await.unwrap();
    assert_eq!(r["result"]["type"], "create_entry");
    assert_eq!(r["result"]["title"], "Verisure (Hjemme)");
    assert_eq!(r["unique_id"], "123456789000");
    let data = r["result"]["data"].clone();
    assert_eq!(data["password"], "secret");
    assert!(data.get("installations").is_none());

    h.call("setup_entry", json!({ "entry": { "entry_id": "e1", "domain": "verisure", "title": "Verisure (Hjemme)", "data": data } })).await.unwrap();
    // the panel, and one device per label even when it is in two lists
    assert_eq!(h.rec.lock().unwrap().devices.len(), 6);
    let alarm = h.entity("123456789000");
    let (state, a) = h.state(&alarm).unwrap();
    assert_eq!(state, "armed_home");
    assert_eq!(a["supported_features"], 3);
    assert_eq!(a["code_format"], "number");
    assert_eq!(a["changed_by"], "Mette");
    assert_eq!(h.state(&h.entity("2BA4 XYZ1-temperature")).unwrap().0, "21.4");
    assert_eq!(h.state(&h.entity("2BA4 XYZ1-humidity")).unwrap().0, "48");
    assert!(h.rec.lock().unwrap().entities.keys().all(|k| !k.contains("3c11_abc2_humidity")));
    assert_eq!(h.state(&h.entity("4D22 DEF3-opening")).unwrap().0, "on");
    let lock = h.entity("5E33 GHI4");
    assert_eq!(h.state(&lock).unwrap().0, "locked");
    assert_eq!(h.state(&h.entity("5E33 GHI4-door")).unwrap().0, "off");
    let plug = h.entity("6F44 JKL5");
    assert_eq!(h.state(&plug).unwrap().0, "off");

    let call = |entity: &str, service: &str, data: Value| json!({ "entry_id": "e1", "entity_id": entity, "service": service, "data": data });
    let err = h.call("entity/call_service", call(&alarm, "alarm_arm_away", json!({}))).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "invalid_argument");
    let err = h.call("entity/call_service", call(&alarm, "alarm_arm_away", json!({ "code": "000000" }))).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "invalid_argument");
    assert!(err["message"].as_str().unwrap().contains("VAL_00819"));
    h.call("entity/call_service", call(&alarm, "alarm_arm_away", json!({ "code": "123456" }))).await.unwrap();
    assert_eq!(h.state(&alarm).unwrap().0, "arming");
    h.call("entity/call_service", call(&lock, "unlock", json!({ "code": 123456 }))).await.unwrap();
    assert_eq!(h.state(&lock).unwrap().0, "unlocking");
    h.call("entity/call_service", call(&plug, "turn_on", json!({}))).await.unwrap();
    assert_eq!(h.state(&plug).unwrap().0, "on");
    let err = h.call("entity/call_service", call(&alarm, "alarm_arm_night", json!({ "code": "1" }))).await.unwrap_err();
    assert_eq!(err["data"]["kind"], "not_supported");

    let sent: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/graphql")
        .flat_map(|r| serde_json::from_slice::<Vec<Value>>(&r.body).unwrap())
        .filter(|op| op["query"].as_str().unwrap().starts_with("mutation"))
        .collect();
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[1]["variables"], json!({ "giid": "123456789000", "code": "123456" }));
    assert!(sent[1]["query"].as_str().unwrap().contains("armStateArmAway(giid: $giid, code: $code)"));
    assert_eq!(sent[2]["operationName"], "DoorUnlock");
    assert_eq!(sent[2]["variables"], json!({ "giid": "123456789000", "deviceLabel": "5E33 GHI4", "input": { "code": "123456" } }));
    assert_eq!(sent[3]["variables"]["state"], true);

    let diag = h.call("diagnostics", json!({ "entry": { "entry_id": "e1", "data": { "password": "secret", "cookies": {} }, "options": { "code": "1234" } } })).await.unwrap();
    assert_eq!(diag["entry"]["password"], "**REDACTED**");
    assert_eq!(diag["options"]["code"], "**REDACTED**");
}

#[tokio::test(flavor = "multi_thread")]
async fn saved_code_and_options() {
    let server = MockServer::start().await;
    cloud(&server).await;
    let h = host::FakeHost::start(Verisure::default());
    let data = json!({ "base_url": server.uri(), "email": "a@b.dk", "password": "secret", "giid": "123456789000", "cookies": { "vs-access": "A1" } });
    h.call("setup_entry", json!({ "entry": { "entry_id": "e1", "domain": "verisure", "data": data, "options": { "code": "123456" } } })).await.unwrap();
    let alarm = h.entity("123456789000");
    let (_, a) = h.state(&alarm).unwrap();
    assert_eq!(a["code_format"], Value::Null);
    assert_eq!(a["code_arm_required"], false);
    h.call("entity/call_service", json!({ "entry_id": "e1", "entity_id": alarm, "service": "alarm_disarm", "data": {} })).await.unwrap();
    assert_eq!(h.state(&alarm).unwrap().0, "disarming");

    let opts = |options: Value, input: Value| json!({ "flow": { "flow_id": "o1", "source": "options", "entry": { "entry_id": "e1", "data": {}, "options": options } }, "step": "init", "input": input });
    let r = h.call("options_flow", opts(json!({ "code": "1111" }), json!({ "code": "", "scan_interval": 5 }))).await.unwrap();
    assert_eq!(r["result"]["data"], json!({ "scan_interval": 30, "code": "1111" }));
    let r = h.call("options_flow", opts(json!({ "code": "1111" }), json!({ "code": "-" }))).await.unwrap();
    assert_eq!(r["result"]["data"], json!({ "scan_interval": 60 }));
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_session_is_renewed_and_saved() {
    let server = MockServer::start().await;
    // the first request finds the session expired
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "errors": [{ "message": "Not Authorized", "data": { "errorCode": "SYS_00004" } }] }])))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET")).and(path("/auth/token")).and(has_cookie("vs-refresh")).respond_with(ResponseTemplate::new(200).insert_header("set-cookie", "vs-access=A2; Path=/")).mount(&server).await;
    Mock::given(method("POST")).and(path("/graphql")).respond_with(graphql_answer).mount(&server).await;
    let h = host::FakeHost::start(Verisure::default());
    let data = json!({ "base_url": server.uri(), "email": "a@b.dk", "password": "secret", "giid": "123456789000", "cookies": { "vs-access": "A1", "vs-refresh": "R1" } });
    h.call("setup_entry", json!({ "entry": { "entry_id": "e1", "domain": "verisure", "data": data } })).await.unwrap();
    assert_eq!(h.state(&h.entity("123456789000")).unwrap().0, "armed_home");
    let saved = h.rec.lock().unwrap().entry_updates.last().cloned().unwrap();
    assert_eq!(saved["cookies"]["vs-access"], "A2");
    assert_eq!(saved["password"], "secret");

    // without a refresh cookie and with two-step login, a new code is needed
    let locked = MockServer::start().await;
    Mock::given(method("POST")).and(path("/graphql")).respond_with(ResponseTemplate::new(401)).mount(&locked).await;
    Mock::given(method("GET")).and(path("/auth/token")).respond_with(ResponseTemplate::new(401)).mount(&locked).await;
    Mock::given(method("POST")).and(path("/auth/login")).respond_with(ResponseTemplate::new(200).set_body_json(fixture("login_stepup.json"))).mount(&locked).await;
    let c = Client::new(Urls::single(&locked.uri()), "a@b.dk", "secret", Default::default());
    assert_eq!(c.graphql(&[json!({})]).await.unwrap_err(), ApiError::Auth);
}
