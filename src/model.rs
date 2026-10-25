//! Verisure's data and how it maps to entity states.
//!
//! Every answer to the overview has `data.installation` with one list; they
//! are merged into one object. Devices are known by their `deviceLabel`, and
//! `area` is the name the user gave them in the app.

use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct Installation {
    pub giid: String,
    pub alias: String,
    pub pin_length: Option<u64>,
}

pub fn installations(answers: &[Value]) -> Vec<Installation> {
    answers
        .iter()
        .flat_map(|a| a["data"]["account"]["installations"].as_array().cloned().unwrap_or_default())
        .filter_map(|i| {
            let giid = i["giid"].as_str()?.to_string();
            let street = i["address"]["street"].as_str().unwrap_or("");
            let alias = i["alias"].as_str().filter(|s| !s.is_empty()).or(Some(street).filter(|s| !s.is_empty())).unwrap_or(&giid).to_string();
            Some(Installation { giid, alias, pin_length: i["pinCodeLength"].as_u64() })
        })
        .collect()
}

/// The overview answers merged into one `installation` object.
pub fn merge(answers: &[Value]) -> Map<String, Value> {
    let mut out = Map::new();
    for a in answers {
        if let Some(m) = a["data"]["installation"].as_object() {
            out.extend(m.clone());
        }
    }
    out
}

fn list<'a>(o: &'a Map<String, Value>, k: &str) -> impl Iterator<Item = &'a Value> {
    o.get(k).and_then(Value::as_array).into_iter().flatten()
}

fn label(v: &Value) -> Option<String> {
    v["device"]["deviceLabel"].as_str().map(String::from)
}

/// A device's name: its area, or the type from the app, or its label.
pub fn device_name(v: &Value) -> String {
    let d = &v["device"];
    d["area"].as_str().filter(|s| !s.is_empty()).or(d["gui"]["label"].as_str()).or(d["deviceLabel"].as_str()).unwrap_or("Verisure").to_string()
}

/// `alarm_control_panel` state.
pub fn alarm_state(o: &Map<String, Value>) -> Option<&'static str> {
    match o.get("armState")?["statusType"].as_str()? {
        "DISARMED" => Some("disarmed"),
        "ARMED_HOME" => Some("armed_home"),
        "ARMED_AWAY" => Some("armed_away"),
        _ => None,
    }
}

/// Who changed the alarm last, if Verisure says.
pub fn alarm_changed_by(o: &Map<String, Value>) -> Option<String> {
    o.get("armState")?["name"].as_str().map(String::from)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Climate {
    pub label: String,
    pub name: String,
    pub temperature: Option<f64>,
    pub humidity: Option<f64>,
}

pub fn climates(o: &Map<String, Value>) -> Vec<Climate> {
    list(o, "climates")
        .filter_map(|c| {
            let humidity = if c["humidityEnabled"].as_bool().unwrap_or(true) { c["humidityValue"].as_f64() } else { None };
            Some(Climate { label: label(c)?, name: device_name(c), temperature: c["temperatureValue"].as_f64(), humidity })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct DoorWindow {
    pub label: String,
    pub name: String,
    pub open: Option<bool>,
}

fn open_closed(s: Option<&str>) -> Option<bool> {
    match s? {
        "OPEN" => Some(true),
        "CLOSE" | "CLOSED" => Some(false),
        _ => None,
    }
}

pub fn door_windows(o: &Map<String, Value>) -> Vec<DoorWindow> {
    list(o, "doorWindows").filter_map(|d| Some(DoorWindow { label: label(d)?, name: device_name(d), open: open_closed(d["state"].as_str()) })).collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct SmartLock {
    pub label: String,
    pub name: String,
    pub locked: Option<bool>,
    pub door_open: Option<bool>,
    pub changed_by: Option<String>,
    pub method: Option<String>,
}

pub fn smart_locks(o: &Map<String, Value>) -> Vec<SmartLock> {
    list(o, "smartLocks")
        .filter_map(|l| {
            let locked = match l["lockStatus"].as_str() {
                Some("LOCKED") => Some(true),
                Some("UNLOCKED") => Some(false),
                _ => None,
            };
            Some(SmartLock {
                label: label(l)?,
                name: device_name(l),
                locked,
                door_open: open_closed(l["doorState"].as_str()),
                changed_by: l["user"]["name"].as_str().map(String::from),
                method: l["lockMethod"].as_str().map(String::from),
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct SmartPlug {
    pub label: String,
    pub name: String,
    pub on: Option<bool>,
}

pub fn smart_plugs(o: &Map<String, Value>) -> Vec<SmartPlug> {
    list(o, "smartplugs")
        .filter_map(|p| {
            let on = match p["currentState"].as_str() {
                Some("ON") => Some(true),
                Some("OFF") => Some(false),
                _ => None,
            };
            Some(SmartPlug { label: label(p)?, name: device_name(p), on })
        })
        .collect()
}

/// The alarm services and the mutation for each, with the state to show
/// until the next read.
pub fn alarm_mutation(service: &str) -> Option<(&'static str, &'static str)> {
    match service {
        "alarm_arm_away" => Some(("armAway", "arming")),
        "alarm_arm_home" => Some(("armHome", "arming")),
        "alarm_disarm" => Some(("disarm", "disarming")),
        _ => None,
    }
}

/// `code_format` for an alarm or lock: `number` while the user must type a
/// code, `None` when one is saved in the options.
pub fn code_format(saved: bool) -> Option<&'static str> {
    if saved {
        None
    } else {
        Some("number")
    }
}
