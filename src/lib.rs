//! Verisure alarm systems as an external integration for HomewrightHQ.
//!
//! The user logs in with the e-mail and password from the My Verisure app.
//! With two-step login Verisure sends a code by SMS or e-mail, the user types
//! it in the next step, and Verisure trusts the integration from then on.
//! The session cookies are saved on the entry so a restart does not need a
//! new code.
//!
//! The installation becomes an alarm panel (home, away, disarm). Yale
//! Doorman locks on the installation become locks, climate sensors give
//! temperature and humidity, door and window sensors become binary sensors
//! and smart plugs become switches. Everything is read with one request a
//! minute.

pub mod api;
pub mod model;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use homewright_plugin::{async_trait, json, Device, Entity, Entry, Error, Flow, FlowReply, Host, Integration, Map, Value};

use api::{ops, ApiError, Client, Cookies, Urls};

pub const DOMAIN: &str = "verisure";
pub const DEFAULT_SCAN_SECONDS: u64 = 60;
const MIN_SCAN_SECONDS: u64 = 30;
const MAX_SCAN_SECONDS: u64 = 600;
/// How long after a command the state is read again.
const SETTLE: Duration = Duration::from_secs(15);
/// `supported_features` for the alarm: arm home (1) and arm away (2).
const ALARM_FEATURES: u64 = 3;

/// Logging to HomewrightHQ's log through stderr.
#[doc(hidden)]
pub fn emit(level: &str, message: String) {
    homewright_plugin::log(level, message);
}

#[macro_export]
macro_rules! info {
    ($($t:tt)*) => { $crate::emit("INFO", format!($($t)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => { $crate::emit("WARN", format!($($t)*)) };
}

fn str_of(m: &Map<String, Value>, k: &str) -> String {
    m.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn login_form(d: &Map<String, Value>, errors: Value) -> FlowReply {
    let mfa = Some(str_of(d, "mfa_type")).filter(|s| !s.is_empty()).unwrap_or_else(|| "phone".into());
    let schema = json!({
        "type": "object",
        "required": ["email", "password"],
        "properties": {
            "email": { "type": "string", "title": "E-mail", "default": str_of(d, "email"), "description": "Den e-mail, du logger ind med i My Verisure-appen" },
            "password": { "type": "string", "format": "password", "title": "Adgangskode" },
            "mfa_type": {
                "type": "string", "title": "Send koden som", "default": mfa,
                "description": "Bruges kun, hvis din konto har totrinslogin",
                "oneOf": [{ "const": "phone", "title": "SMS" }, { "const": "email", "title": "E-mail" }]
            }
        }
    });
    FlowReply::form_with_errors("user", schema, errors)
}

fn mfa_form(errors: Value) -> FlowReply {
    let schema = json!({
        "type": "object",
        "required": ["code"],
        "properties": {
            "code": { "type": "string", "title": "Kode", "description": "Koden, Verisure lige har sendt dig" }
        }
    });
    let mut r = FlowReply::form_with_errors("mfa", schema, errors);
    r.result["description"] = json!("Verisure har sendt en kode. Skriv den her. Bagefter husker Verisure integrationen, så den ikke skal bruge en ny kode ved genstart.");
    r
}

fn installation_form(list: &[model::Installation]) -> FlowReply {
    let options: Vec<Value> = list.iter().map(|i| json!({ "const": i.giid, "title": i.alias })).collect();
    let schema = json!({
        "type": "object",
        "required": ["giid"],
        "properties": { "giid": { "type": "string", "title": "Installation", "default": list[0].giid, "oneOf": options } }
    });
    FlowReply::form("installation", schema)
}

fn options_schema(seconds: u64, code_saved: bool) -> Value {
    json!({
        "type": "object",
        "properties": {
            "code": {
                "type": "string", "format": "password", "title": "Alarmkode",
                "description": if code_saved { "Gemt. Lad feltet stå tomt for at beholde den, eller skriv - for at slette den." } else { "Gemmes, så alarm og låse kan styres uden at skrive koden hver gang. Lad feltet stå tomt for at skrive koden ved hver kommando." }
            },
            "scan_interval": { "type": "integer", "title": "Sekunder mellem opdateringer", "default": seconds, "minimum": MIN_SCAN_SECONDS, "maximum": MAX_SCAN_SECONDS }
        }
    })
}

/// Endpoints; tests put a mock URL in the entry or flow data.
fn urls(get: impl Fn(&str) -> Option<String>) -> Urls {
    get("base_url").map(|b| Urls::single(&b)).unwrap_or_default()
}

fn cookies_of(m: &Map<String, Value>) -> Cookies {
    m.get("cookies").and_then(Value::as_object).map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect()).unwrap_or_default()
}

pub fn scan_interval(entry: &Entry) -> Duration {
    Duration::from_secs(entry.get("scan_interval").and_then(Value::as_u64).unwrap_or(DEFAULT_SCAN_SECONDS).clamp(MIN_SCAN_SECONDS, MAX_SCAN_SECONDS))
}

#[derive(Default)]
pub struct Verisure {
    accounts: Mutex<HashMap<String, Arc<Account>>>,
}

impl Verisure {
    fn account(&self, entry_id: &str) -> Result<Arc<Account>, Error> {
        self.accounts.lock().unwrap().get(entry_id).cloned().ok_or_else(|| Error::unavailable("opsætningen kører ikke"))
    }
}

/// After login: pick the installation, or finish when there is one.
async fn after_login(flow: &Flow, entry_data: Map<String, Value>, mut d: Map<String, Value>, client: &Client) -> Result<FlowReply, Error> {
    d.insert("cookies".into(), json!(client.cookies()));
    let list = match client.graphql(&[ops::installations(&str_of(&d, "email"))]).await {
        Ok(a) => model::installations(&a),
        Err(e) => {
            info!("Verisure: {e}");
            return Ok(login_form(&d, json!({ "base": "cannot_connect" })).with_data(d));
        }
    };
    if flow.source == "reauth" {
        let mut data = entry_data;
        for k in ["email", "password", "mfa_type", "cookies"] {
            if let Some(v) = d.get(k) {
                data.insert(k.into(), v.clone());
            }
        }
        return Ok(FlowReply::create("", Value::Object(data)));
    }
    match list.len() {
        0 => Ok(FlowReply::abort("no_devices_found")),
        1 => Ok(create(d, &list[0])),
        _ => {
            d.insert("installations".into(), json!(list.iter().map(|i| json!({ "giid": i.giid, "alias": i.alias })).collect::<Vec<_>>()));
            Ok(installation_form(&list).with_data(d))
        }
    }
}

fn create(mut d: Map<String, Value>, i: &model::Installation) -> FlowReply {
    d.remove("installations");
    d.insert("giid".into(), json!(i.giid));
    FlowReply::create(&format!("Verisure ({})", i.alias), Value::Object(d)).with_unique_id(i.giid.clone())
}

#[async_trait]
impl Integration for Verisure {
    async fn config_flow(&self, _host: &Host, flow: Flow, step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        let entry_data = flow.entry.as_ref().and_then(|e| e.data.as_object().cloned()).unwrap_or_default();
        let mut d = flow.data.clone();
        match (step, input) {
            ("user", None) => Ok(login_form(&d, json!({}))),
            ("reauth", None) => {
                for (k, v) in &entry_data {
                    d.entry(k.clone()).or_insert(v.clone());
                }
                Ok(login_form(&d, json!({})).with_data(d))
            }
            ("user", Some(v)) => {
                d.insert("email".into(), json!(v["email"].as_str().unwrap_or("").trim()));
                d.insert("password".into(), json!(v["password"].as_str().unwrap_or("")));
                d.insert("mfa_type".into(), json!(v["mfa_type"].as_str().filter(|s| *s == "email").unwrap_or("phone")));
                let client = Client::new(urls(|k| d.get(k).and_then(Value::as_str).map(String::from)), &str_of(&d, "email"), &str_of(&d, "password"), cookies_of(&d));
                match client.login().await {
                    Ok(()) => after_login(&flow, entry_data, d, &client).await,
                    Err(ApiError::MfaRequired) => {
                        if let Err(e) = client.request_mfa(&str_of(&d, "mfa_type")).await {
                            info!("Verisure: {e}");
                            return Ok(login_form(&d, json!({ "base": "cannot_connect" })).with_data(d));
                        }
                        d.insert("cookies".into(), json!(client.cookies()));
                        Ok(mfa_form(json!({})).with_data(d))
                    }
                    Err(e) => {
                        info!("Verisure: {e}");
                        let code = if e == ApiError::Auth { "invalid_auth" } else { "cannot_connect" };
                        Ok(login_form(&d, json!({ "base": code })).with_data(d))
                    }
                }
            }
            ("mfa", Some(v)) => {
                let code: String = v["code"].as_str().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
                let client = Client::new(urls(|k| d.get(k).and_then(Value::as_str).map(String::from)), &str_of(&d, "email"), &str_of(&d, "password"), cookies_of(&d));
                match client.validate_mfa(&code).await {
                    Ok(()) => after_login(&flow, entry_data, d, &client).await,
                    Err(e) => {
                        info!("Verisure: {e}");
                        let err = if e == ApiError::Auth { "invalid_code" } else { "cannot_connect" };
                        Ok(mfa_form(json!({ "base": err })).with_data(d))
                    }
                }
            }
            ("installation", Some(v)) => {
                let giid = v["giid"].as_str().unwrap_or("");
                let list: Vec<model::Installation> = d
                    .get("installations")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|i| model::Installation { giid: i["giid"].as_str().unwrap_or("").into(), alias: i["alias"].as_str().unwrap_or("").into(), pin_length: None })
                    .collect();
                match list.iter().find(|i| i.giid == giid) {
                    Some(i) => Ok(create(d.clone(), i)),
                    None => Err(Error::invalid(format!("ukendt installation {giid}"))),
                }
            }
            (s, _) => Err(Error::invalid(format!("ukendt trin {s}"))),
        }
    }

    async fn options_flow(&self, _host: &Host, flow: Flow, _step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        let entry = flow.entry.clone().unwrap_or_default();
        let current = scan_interval(&entry).as_secs();
        let saved = entry.options.get("code").and_then(Value::as_str).unwrap_or("").to_string();
        match input {
            None => Ok(FlowReply::form("init", options_schema(current, !saved.is_empty()))),
            Some(v) => {
                let typed = v["code"].as_str().unwrap_or("").trim();
                let code = match typed {
                    "" => saved,
                    "-" => String::new(),
                    c => c.to_string(),
                };
                let s = v["scan_interval"].as_u64().unwrap_or(current).clamp(MIN_SCAN_SECONDS, MAX_SCAN_SECONDS);
                let mut o = json!({ "scan_interval": s });
                if !code.is_empty() {
                    o["code"] = json!(code);
                }
                Ok(FlowReply::create("", o))
            }
        }
    }

    async fn setup_entry(&self, host: &Host, entry: Entry) -> Result<(), Error> {
        let entry_id = entry.entry_id.clone();
        let data = entry.data.as_object().cloned().unwrap_or_default();
        let giid = str_of(&data, "giid");
        if giid.is_empty() {
            return Err(Error::auth_failed("ingen installation; log ind igen"));
        }
        let client = Arc::new(Client::new(urls(|k| entry.get_str(k).map(String::from)), &str_of(&data, "email"), &str_of(&data, "password"), cookies_of(&data)));
        let code = entry.options.get("code").and_then(Value::as_str).filter(|c| !c.is_empty()).map(String::from);
        let account = Arc::new(Account {
            entry_id: entry_id.clone(),
            host: host.clone(),
            client: client.clone(),
            giid: giid.clone(),
            code,
            data: Mutex::new(data),
            overview: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            entities: Mutex::new(vec![]),
            task: Mutex::new(None),
        });
        let answers = client.graphql(&ops::overview(&giid)).await;
        account.persist().await;
        let overview = match answers {
            Ok(a) => model::merge(&a),
            Err(ApiError::Auth) => return Err(Error::auth_failed("Verisure afviste login")),
            Err(e) => return Err(Error::not_ready(e)),
        };
        let _ = host.delete_issue(&entry_id, "auth").await;
        account.register(&entry.title, &overview).await?;
        *account.overview.lock().unwrap() = Some(overview);
        account.publish().await;

        let interval = scan_interval(&entry);
        let acc = account.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                acc.poll().await;
            }
        });
        *account.task.lock().unwrap() = Some(task);
        if let Some(old) = self.accounts.lock().unwrap().insert(entry_id, account) {
            old.stop();
        }
        Ok(())
    }

    async fn unload_entry(&self, _host: &Host, entry_id: &str) -> Result<(), Error> {
        if let Some(a) = self.accounts.lock().unwrap().remove(entry_id) {
            a.stop();
        }
        Ok(())
    }

    async fn entity_service(&self, _host: &Host, entry_id: &str, entity_id: &str, service: &str, data: Map<String, Value>) -> Result<(), Error> {
        let account = self.account(entry_id)?;
        account.call_service(entity_id, service, &data).await
    }

    async fn diagnostics(&self, _host: &Host, entry: Entry) -> Value {
        let mut d = entry.data.clone();
        if let Value::Object(m) = &mut d {
            for k in ["password", "cookies"] {
                if m.contains_key(k) {
                    m.insert(k.into(), json!("**REDACTED**"));
                }
            }
        }
        let mut o = entry.options.clone();
        if let Value::Object(m) = &mut o {
            if m.contains_key("code") {
                m.insert("code".into(), json!("**REDACTED**"));
            }
        }
        let overview = self.accounts.lock().unwrap().get(&entry.entry_id).and_then(|a| a.overview.lock().unwrap().clone());
        json!({ "entry": d, "options": o, "overview": overview })
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Alarm,
    Temperature(String),
    Humidity(String),
    DoorWindow(String),
    Lock(String),
    LockDoor(String),
    Plug(String),
}

struct Account {
    entry_id: String,
    host: Host,
    client: Arc<Client>,
    giid: String,
    /// The alarm code from the options.
    code: Option<String>,
    data: Mutex<Map<String, Value>>,
    /// The merged overview, `None` after a failed read.
    overview: Mutex<Option<Map<String, Value>>>,
    /// entity_id -> state shown until the next read after a command.
    pending: Mutex<HashMap<String, String>>,
    entities: Mutex<Vec<(String, Kind)>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// A state and its attributes.
type StateUpdate = (String, Map<String, Value>);

fn on_off(b: bool) -> String {
    if b { "on" } else { "off" }.to_string()
}

impl Account {
    fn stop(&self) {
        if let Some(t) = self.task.lock().unwrap().take() {
            t.abort();
        }
    }

    /// Save the session cookies when Verisure changed them.
    async fn persist(&self) {
        if !self.client.take_dirty() {
            return;
        }
        let data = {
            let mut d = self.data.lock().unwrap();
            d.insert("cookies".into(), json!(self.client.cookies()));
            d.clone()
        };
        if let Err(e) = self.host.update_entry_data(&self.entry_id, Value::Object(data)).await {
            warn!("kunne ikke gemme Verisures session: {e}");
        }
    }

    async fn register(&self, title: &str, o: &Map<String, Value>) -> Result<(), Error> {
        let host = &self.host;
        let giid = &self.giid;
        let panel = host
            .register_device(
                &self.entry_id,
                Device {
                    identifiers: vec![(DOMAIN.into(), giid.clone())],
                    name: if title.is_empty() { "Verisure".into() } else { title.to_string() },
                    manufacturer: Some("Verisure".into()),
                    model: Some("Alarm".into()),
                    ..Default::default()
                },
            )
            .await?;
        let mut list: Vec<(Kind, Entity, bool)> = vec![];
        let mut alarm = Entity::new("alarm_control_panel", giid.clone()).device(&panel);
        alarm.capabilities = json!({ "supported_features": ALARM_FEATURES });
        list.push((Kind::Alarm, alarm, true));

        // one device per deviceLabel, also when it shows up in several lists
        let mut devices: HashMap<String, String> = HashMap::new();
        let mut wanted: Vec<(String, String, &str)> = vec![];
        for c in model::climates(o) {
            wanted.push((c.label, c.name, "Klimasensor"));
        }
        for d in model::door_windows(o) {
            wanted.push((d.label, d.name, "Dør/vindue-sensor"));
        }
        for l in model::smart_locks(o) {
            wanted.push((l.label, l.name, "Smart lås"));
        }
        for p in model::smart_plugs(o) {
            wanted.push((p.label, p.name, "Smart stik"));
        }
        let mut seen = HashSet::new();
        for (label, name, kind) in wanted {
            if !seen.insert(label.clone()) {
                continue;
            }
            let id = host
                .register_device(
                    &self.entry_id,
                    Device {
                        identifiers: vec![(DOMAIN.into(), label.clone())],
                        name,
                        manufacturer: Some("Verisure".into()),
                        model: Some(kind.into()),
                        serial_number: Some(label.clone()),
                        via_device: Some((DOMAIN.into(), giid.clone())),
                        ..Default::default()
                    },
                )
                .await?;
            devices.insert(label, id);
        }
        let dev = |label: &str| devices.get(label).cloned().unwrap_or_else(|| panel.clone());
        for c in model::climates(o) {
            let d = dev(&c.label);
            if c.temperature.is_some() {
                list.push((
                    Kind::Temperature(c.label.clone()),
                    Entity::new("sensor", format!("{}-temperature", c.label)).name("Temperatur").device(&d).device_class("temperature").unit("°C").state_class("measurement"),
                    false,
                ));
            }
            if c.humidity.is_some() {
                list.push((
                    Kind::Humidity(c.label.clone()),
                    Entity::new("sensor", format!("{}-humidity", c.label)).name("Luftfugtighed").device(&d).device_class("humidity").unit("%").state_class("measurement"),
                    false,
                ));
            }
        }
        for w in model::door_windows(o) {
            list.push((Kind::DoorWindow(w.label.clone()), Entity::new("binary_sensor", format!("{}-opening", w.label)).device(&dev(&w.label)).device_class("opening"), false));
        }
        for l in model::smart_locks(o) {
            let d = dev(&l.label);
            list.push((Kind::Lock(l.label.clone()), Entity::new("lock", l.label.clone()).device(&d), true));
            if l.door_open.is_some() {
                list.push((Kind::LockDoor(l.label.clone()), Entity::new("binary_sensor", format!("{}-door", l.label)).name("Dør").device(&d).device_class("door"), false));
            }
        }
        for p in model::smart_plugs(o) {
            list.push((Kind::Plug(p.label.clone()), Entity::new("switch", p.label.clone()).device(&dev(&p.label)).device_class("outlet"), true));
        }
        for (kind, e, handler) in list {
            if let Some(entity_id) = host.add_entity(&self.entry_id, e, handler).await? {
                self.entities.lock().unwrap().push((entity_id, kind));
            }
        }
        Ok(())
    }

    async fn publish(&self) {
        let code_format = model::code_format(self.code.is_some());
        let updates: Vec<(String, Option<StateUpdate>)> = {
            let overview = self.overview.lock().unwrap();
            let pending = self.pending.lock().unwrap();
            self.entities
                .lock()
                .unwrap()
                .iter()
                .map(|(entity_id, kind)| {
                    let Some(o) = overview.as_ref() else { return (entity_id.clone(), None) };
                    let mut a = Map::new();
                    let state: Option<String> = match kind {
                        Kind::Alarm => {
                            a.insert("supported_features".into(), json!(ALARM_FEATURES));
                            a.insert("code_format".into(), json!(code_format));
                            a.insert("code_arm_required".into(), json!(self.code.is_none()));
                            a.insert("changed_by".into(), json!(model::alarm_changed_by(o)));
                            model::alarm_state(o).map(String::from)
                        }
                        Kind::Temperature(l) => model::climates(o).into_iter().find(|c| &c.label == l).and_then(|c| c.temperature).map(|t| t.to_string()),
                        Kind::Humidity(l) => model::climates(o).into_iter().find(|c| &c.label == l).and_then(|c| c.humidity).map(|t| t.to_string()),
                        Kind::DoorWindow(l) => model::door_windows(o).into_iter().find(|d| &d.label == l).and_then(|d| d.open).map(on_off),
                        Kind::Lock(l) => {
                            let lock = model::smart_locks(o).into_iter().find(|d| &d.label == l);
                            a.insert("code_format".into(), json!(code_format));
                            if let Some(lock) = &lock {
                                a.insert("changed_by".into(), json!(lock.changed_by));
                                a.insert("method".into(), json!(lock.method));
                            }
                            lock.and_then(|d| d.locked).map(|b| if b { "locked" } else { "unlocked" }.to_string())
                        }
                        Kind::LockDoor(l) => model::smart_locks(o).into_iter().find(|d| &d.label == l).and_then(|d| d.door_open).map(on_off),
                        Kind::Plug(l) => model::smart_plugs(o).into_iter().find(|d| &d.label == l).and_then(|d| d.on).map(on_off),
                    };
                    let state = pending.get(entity_id).cloned().or(state).unwrap_or_else(|| "unknown".into());
                    (entity_id.clone(), Some((state, a)))
                })
                .collect()
        };
        for (entity_id, value) in updates {
            let res = match value {
                None => self.host.set_unavailable(&entity_id).await,
                Some((state, attrs)) => self.host.set_state(&entity_id, state, attrs).await,
            };
            if let Err(e) = res {
                warn!("kunne ikke opdatere {entity_id}: {e}");
            }
        }
    }

    async fn poll(&self) {
        let answers = self.client.graphql(&ops::overview(&self.giid)).await;
        self.persist().await;
        match answers {
            Ok(a) => {
                let _ = self.host.delete_issue(&self.entry_id, "auth").await;
                *self.overview.lock().unwrap() = Some(model::merge(&a));
                self.pending.lock().unwrap().clear();
            }
            Err(e) => {
                self.report(&e).await;
                *self.overview.lock().unwrap() = None;
            }
        }
        self.publish().await;
    }

    async fn report(&self, e: &ApiError) {
        match e {
            ApiError::Auth | ApiError::MfaRequired => {
                let r = self
                    .host
                    .create_issue(&self.entry_id, "auth", "error", "Verisure: login afvist", "Verisure afviste login, eller sessionen kunne ikke fornyes uden en ny kode. Godkend integrationen igen.")
                    .await;
                if let Err(e) = r {
                    warn!("kunne ikke oprette problem: {e}");
                }
            }
            other => warn!("{other}"),
        }
    }

    fn code(&self, data: &Map<String, Value>) -> Result<String, Error> {
        let given = data.get("code").and_then(|c| c.as_str().map(String::from).or_else(|| c.as_u64().map(|n| n.to_string()))).filter(|c| !c.is_empty());
        given.or_else(|| self.code.clone()).ok_or_else(|| Error::invalid("alarmkoden mangler; skriv den, eller gem den under integrationens indstillinger"))
    }

    async fn call_service(self: &Arc<Self>, entity_id: &str, service: &str, data: &Map<String, Value>) -> Result<(), Error> {
        let kind = self.entities.lock().unwrap().iter().find(|(e, _)| e == entity_id).map(|(_, k)| k.clone()).ok_or_else(|| Error::new("not_found", entity_id))?;
        let giid = &self.giid;
        let (op, shown) = match (&kind, service) {
            (Kind::Alarm, s) => {
                let (m, shown) = model::alarm_mutation(s).ok_or_else(|| Error::not_supported(s))?;
                (ops::arm(m, giid, &self.code(data)?), shown)
            }
            (Kind::Lock(l), "lock") => (ops::door_lock("DoorLock", giid, l, &self.code(data)?), "locking"),
            (Kind::Lock(l), "unlock") => (ops::door_lock("DoorUnlock", giid, l, &self.code(data)?), "unlocking"),
            (Kind::Plug(l), "turn_on") => (ops::smart_plug_set(giid, l, true), "on"),
            (Kind::Plug(l), "turn_off") => (ops::smart_plug_set(giid, l, false), "off"),
            _ => return Err(Error::not_supported(service)),
        };
        let res = self.client.graphql(&[op]).await;
        self.persist().await;
        if let Err(e) = res {
            self.report(&e).await;
            return Err(match e {
                ApiError::Refused(m) => Error::invalid(m),
                ApiError::Auth | ApiError::MfaRequired => Error::auth_failed(e),
                other => Error::failed(other),
            });
        }
        self.pending.lock().unwrap().insert(entity_id.to_string(), shown.to_string());
        self.publish().await;
        let me = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SETTLE).await;
            me.poll().await;
        });
        Ok(())
    }
}
