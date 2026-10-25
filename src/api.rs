//! Verisure's app API, the one the My Verisure app uses, as python-verisure
//! (vsure) describes it.
//!
//! Login is HTTP basic auth against `/auth/login` and gives session cookies.
//! With two-step login switched on, Verisure answers with a `stepUpToken`
//! instead; a code is then sent by SMS or e-mail (`/auth/mfa`), checked with
//! `/auth/mfa/validate`, and `/auth/trust` gives a cookie that lets later
//! logins skip the code. Everything else is GraphQL at `/graphql`, several
//! operations in one request. Each kind of server has two hosts; the second
//! is tried when the first does not answer.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Mutex;

pub const AUTH_URLS: [&str; 2] = ["https://automation01.verisure.com", "https://automation02.verisure.com"];
pub const GRAPHQL_URLS: [&str; 2] = ["https://m-api01.verisure.com", "https://m-api02.verisure.com"];
/// What python-verisure sends as `APPLICATION_ID`.
const APPLICATION_ID: &str = "PS_PYTHON";

#[derive(Clone, Debug, PartialEq)]
pub struct Urls {
    pub auth: Vec<String>,
    pub graphql: Vec<String>,
}

impl Default for Urls {
    fn default() -> Self {
        Self { auth: AUTH_URLS.iter().map(|s| s.to_string()).collect(), graphql: GRAPHQL_URLS.iter().map(|s| s.to_string()).collect() }
    }
}

impl Urls {
    /// One address for both, for tests.
    pub fn single(base: &str) -> Self {
        Self { auth: vec![base.into()], graphql: vec![base.into()] }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// Wrong e-mail, password or code, or a session that cannot be renewed.
    Auth,
    /// The login needs a code from SMS or e-mail.
    MfaRequired,
    /// Verisure refused a command, e.g. a wrong alarm code.
    Refused(String),
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Auth => write!(f, "Verisure afviste login"),
            ApiError::MfaRequired => write!(f, "Verisure kræver en kode fra SMS eller e-mail"),
            ApiError::Refused(e) => write!(f, "Verisure afviste kommandoen: {e}"),
            ApiError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        ApiError::Other(e.to_string())
    }
}

/// One GraphQL operation as Verisure takes it.
pub fn op(name: &str, variables: Value, query: &str) -> Value {
    json!({ "operationName": name, "variables": variables, "query": query })
}

pub type Cookies = BTreeMap<String, String>;

pub struct Client {
    http: reqwest::Client,
    urls: Urls,
    email: String,
    password: String,
    cookies: std::sync::Mutex<Cookies>,
    /// The cookies changed and should be saved.
    dirty: AtomicBool,
    /// Only one login or renewal at a time.
    login_lock: Mutex<()>,
}

struct Answer {
    status: reqwest::StatusCode,
    body: String,
}

impl Client {
    pub fn new(urls: Urls, email: &str, password: &str, cookies: Cookies) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap_or_default();
        Self { http, urls, email: email.into(), password: password.into(), cookies: std::sync::Mutex::new(cookies), dirty: AtomicBool::new(false), login_lock: Mutex::new(()) }
    }

    pub fn cookies(&self) -> Cookies {
        self.cookies.lock().unwrap().clone()
    }

    /// True once after the cookies changed.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }

    fn cookie_header(&self) -> String {
        self.cookies.lock().unwrap().iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
    }

    fn keep_cookies(&self, res: &reqwest::Response) {
        let mut jar = self.cookies.lock().unwrap();
        for h in res.headers().get_all(reqwest::header::SET_COOKIE) {
            let Ok(s) = h.to_str() else { continue };
            let mut parts = s.split(';');
            let Some((name, value)) = parts.next().and_then(|p| p.trim().split_once('=')) else { continue };
            let expired = parts.any(|p| p.trim().eq_ignore_ascii_case("max-age=0"));
            let changed = if value.is_empty() || expired { jar.remove(name).is_some() } else { jar.insert(name.to_string(), value.to_string()).as_deref() != Some(value) };
            if changed {
                self.dirty.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Send to each host in turn until one answers below 500.
    async fn send(&self, bases: &[String], method: reqwest::Method, path: &str, basic: bool, body: Option<&Value>) -> Result<Answer, ApiError> {
        let mut last = ApiError::Other("ingen adresse".into());
        for base in bases {
            let mut req = self.http.request(method.clone(), format!("{}{path}", base.trim_end_matches('/'))).header("APPLICATION_ID", APPLICATION_ID).header("Accept", "application/json");
            let cookie = self.cookie_header();
            if !cookie.is_empty() {
                req = req.header(reqwest::header::COOKIE, cookie);
            }
            if basic {
                req = req.basic_auth(&self.email, Some(&self.password));
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            match req.send().await {
                Ok(res) => {
                    self.keep_cookies(&res);
                    let status = res.status();
                    let body = res.text().await.unwrap_or_default();
                    if status.is_server_error() {
                        last = ApiError::Other(format!("HTTP {status}: {}", body.chars().take(200).collect::<String>()));
                        continue;
                    }
                    return Ok(Answer { status, body });
                }
                Err(e) => last = e.into(),
            }
        }
        Err(last)
    }

    /// Log in with e-mail and password and any saved trust cookie.
    pub async fn login(&self) -> Result<(), ApiError> {
        let a = self.send(&self.urls.auth, reqwest::Method::POST, "/auth/login", true, None).await?;
        if matches!(a.status.as_u16(), 400 | 401 | 403) {
            return Err(ApiError::Auth);
        }
        if !a.status.is_success() {
            return Err(ApiError::Other(format!("login: HTTP {}", a.status)));
        }
        if a.body.contains("stepUpToken") {
            return Err(ApiError::MfaRequired);
        }
        Ok(())
    }

    /// Ask Verisure to send a code. `kind` is `phone` (SMS) or `email`.
    pub async fn request_mfa(&self, kind: &str) -> Result<(), ApiError> {
        let a = self.send(&self.urls.auth, reqwest::Method::POST, &format!("/auth/mfa?type={kind}"), false, None).await?;
        if !a.status.is_success() {
            return Err(ApiError::Other(format!("mfa: HTTP {}", a.status)));
        }
        Ok(())
    }

    /// Check the code and ask Verisure to trust this client from now on.
    pub async fn validate_mfa(&self, code: &str) -> Result<(), ApiError> {
        let a = self.send(&self.urls.auth, reqwest::Method::POST, "/auth/mfa/validate", false, Some(&json!({ "token": code }))).await?;
        if matches!(a.status.as_u16(), 400 | 401 | 403) {
            return Err(ApiError::Auth);
        }
        if !a.status.is_success() {
            return Err(ApiError::Other(format!("mfa/validate: HTTP {}", a.status)));
        }
        let t = self.send(&self.urls.auth, reqwest::Method::POST, "/auth/trust", false, None).await?;
        if !t.status.is_success() {
            crate::warn!("Verisure ville ikke huske klienten (HTTP {}); næste login kan kræve en ny kode", t.status);
        }
        Ok(())
    }

    /// Renew the access cookie with the refresh cookie.
    async fn renew(&self) -> Result<(), ApiError> {
        let a = self.send(&self.urls.auth, reqwest::Method::GET, "/auth/token", false, None).await?;
        if a.status.is_success() {
            Ok(())
        } else {
            Err(ApiError::Auth)
        }
    }

    async fn relogin(&self) -> Result<(), ApiError> {
        let _guard = self.login_lock.lock().await;
        if self.renew().await.is_ok() {
            return Ok(());
        }
        match self.login().await {
            // a code cannot be typed in the background
            Err(ApiError::MfaRequired) => Err(ApiError::Auth),
            r => r,
        }
    }

    /// Run GraphQL operations in one request. The answers come in the same
    /// order. A session that has run out is renewed once.
    pub async fn graphql(&self, ops: &[Value]) -> Result<Vec<Value>, ApiError> {
        let body = Value::Array(ops.to_vec());
        for attempt in 0..2 {
            let a = self.send(&self.urls.graphql, reqwest::Method::POST, "/graphql", false, Some(&body)).await?;
            let parsed: Value = serde_json::from_str(&a.body).unwrap_or(Value::Null);
            let answers: Vec<Value> = match parsed {
                Value::Array(v) => v,
                Value::Null => vec![],
                v => vec![v],
            };
            let expired = a.status == reqwest::StatusCode::UNAUTHORIZED || answers.iter().any(auth_error);
            if expired {
                if attempt == 0 {
                    self.relogin().await?;
                    continue;
                }
                return Err(ApiError::Auth);
            }
            if !a.status.is_success() {
                return Err(ApiError::Other(format!("graphql: HTTP {}: {}", a.status, a.body.chars().take(200).collect::<String>())));
            }
            if let Some(e) = answers.iter().find_map(error_message) {
                return Err(ApiError::Refused(e));
            }
            return Ok(answers);
        }
        Err(ApiError::Auth)
    }
}

fn errors(a: &Value) -> impl Iterator<Item = &Value> {
    a["errors"].as_array().into_iter().flatten()
}

/// Errors that mean the session is gone, as python-verisure checks them.
fn auth_error(a: &Value) -> bool {
    errors(a).any(|e| {
        let code = e["data"]["errorCode"].as_str().unwrap_or("");
        let msg = e["message"].as_str().unwrap_or("").to_lowercase();
        code == "SYS_00004" || code == "SYS_00001" || msg.contains("not authorized") || msg.contains("unauthenticated")
    })
}

fn error_message(a: &Value) -> Option<String> {
    errors(a).next().map(|e| {
        let code = e["data"]["errorCode"].as_str().unwrap_or("");
        let msg = e["data"]["errorMessage"].as_str().or(e["message"].as_str()).unwrap_or("ukendt fejl");
        if code.is_empty() {
            msg.to_string()
        } else {
            format!("{code}: {msg}")
        }
    })
}

/// The GraphQL operations, with the queries python-verisure sends.
pub mod ops {
    use super::op;
    use serde_json::{json, Value};

    pub fn installations(email: &str) -> Value {
        op(
            "fetchAllInstallations",
            json!({ "email": email }),
            "query fetchAllInstallations($email: String!){\n  account(email: $email) {\n    installations {\n      giid\n      alias\n      customerType\n      dealerId\n      subsidiary\n      pinCodeLength\n      locale\n      address {\n        street\n        city\n        postalNumber\n        __typename\n      }\n      __typename\n    }\n    __typename\n  }\n}\n",
        )
    }

    pub fn arm_state(giid: &str) -> Value {
        op(
            "ArmState",
            json!({ "giid": giid }),
            "query ArmState($giid: String!) {\n  installation(giid: $giid) {\n    armState {\n      type\n      statusType\n      date\n      name\n      changedVia\n      allowedForFirstLine\n      allowed\n      errorCodes {\n        value\n        message\n        __typename\n      }\n      __typename\n    }\n    __typename\n  }\n}\n",
        )
    }

    pub fn climate(giid: &str) -> Value {
        op(
            "Climate",
            json!({ "giid": giid }),
            "query Climate($giid: String!) {\n  installation(giid: $giid) {\n    climates {\n      device {\n        deviceLabel\n        area\n        gui {\n          label\n          __typename\n        }\n        __typename\n      }\n      humidityEnabled\n      humidityTimestamp\n      humidityValue\n      temperatureTimestamp\n      temperatureValue\n      __typename\n    }\n    __typename\n  }\n}\n",
        )
    }

    pub fn door_window(giid: &str) -> Value {
        op(
            "DoorWindow",
            json!({ "giid": giid }),
            "query DoorWindow($giid: String!) {\n  installation(giid: $giid) {\n    doorWindows {\n      device {\n        deviceLabel\n        area\n        __typename\n      }\n      type\n      state\n      wired\n      reportTime\n      __typename\n    }\n    __typename\n  }\n}\n",
        )
    }

    pub fn smart_locks(giid: &str) -> Value {
        op(
            "SmartLock",
            json!({ "giid": giid }),
            "query SmartLock($giid: String!) {\n  installation(giid: $giid) {\n    smartLocks {\n      lockStatus\n      doorState\n      lockMethod\n      eventTime\n      doorLockType\n      secureMode\n      device {\n        deviceLabel\n        area\n        __typename\n      }\n      user {\n        name\n        __typename\n      }\n      __typename\n    }\n    __typename\n  }\n}\n",
        )
    }

    pub fn smart_plugs(giid: &str) -> Value {
        op(
            "SmartPlug",
            json!({ "giid": giid }),
            "query SmartPlug($giid: String!) {\n  installation(giid: $giid) {\n    smartplugs {\n      device {\n        deviceLabel\n        area\n        gui {\n          label\n          __typename\n        }\n        __typename\n      }\n      currentState\n      icon\n      isHazardous\n      __typename\n    }\n    __typename\n  }\n}\n",
        )
    }

    /// `armAway`, `armHome` or `disarm`.
    pub fn arm(mode: &str, giid: &str, code: &str) -> Value {
        let field = match mode {
            "armAway" => "armStateArmAway",
            "armHome" => "armStateArmHome",
            _ => "armStateDisarm",
        };
        op(mode, json!({ "giid": giid, "code": code }), &format!("mutation {mode}($giid: String!, $code: String!) {{\n  {field}(giid: $giid, code: $code)\n}}\n"))
    }

    /// `DoorLock` or `DoorUnlock`.
    pub fn door_lock(name: &str, giid: &str, label: &str, code: &str) -> Value {
        op(
            name,
            json!({ "giid": giid, "deviceLabel": label, "input": { "code": code } }),
            &format!("mutation {name}($giid: String!, $deviceLabel: String!, $input: LockDoorInput!) {{\n  {name}(giid: $giid, deviceLabel: $deviceLabel, input: $input)\n}}\n"),
        )
    }

    pub fn smart_plug_set(giid: &str, label: &str, on: bool) -> Value {
        op(
            "UpdateState",
            json!({ "giid": giid, "deviceLabel": label, "state": on }),
            "mutation UpdateState($giid: String!, $deviceLabel: String!, $state: Boolean!) {\n  SmartPlugSetState(giid: $giid, input: [{deviceLabel: $deviceLabel, state: $state}])\n}\n",
        )
    }

    /// Everything the integration shows, in one request.
    pub fn overview(giid: &str) -> Vec<Value> {
        vec![arm_state(giid), climate(giid), door_window(giid), smart_locks(giid), smart_plugs(giid)]
    }
}
