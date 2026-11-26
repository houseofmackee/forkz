//! AWS profiles, as the AWS CLI and SDKs read them from `~/.aws/config`
//! and `~/.aws/credentials`: keys (with a session token or not), keys from
//! a `credential_process`, roles assumed from another profile (asking for
//! an MFA code if the profile has `mfa_serial`), and IAM Identity Center
//! (SSO). For SSO, a sign-in the AWS CLI made (`aws sso login`) is used
//! while it lasts and renewed when it can be; otherwise forkz signs in
//! itself in the browser, and keeps the sign-in where the CLI does (so
//! each sees the other's).

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::http;
use super::sftp::Prompter;

/// Keys to sign requests with.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub session_token: Option<String>,
    /// When they stop working (None: they do not).
    pub expires: Option<SystemTime>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the secret parts.
        f.debug_struct("Credentials").field("access_key", &self.access_key).field("expires", &self.expires).finish()
    }
}

impl Credentials {
    /// Whether they stop working within `margin`.
    pub fn expiring(&self, margin: Duration) -> bool {
        self.expires.is_some_and(|t| t <= SystemTime::now() + margin)
    }
}

/// Asks the user while signing in (from the connecting thread).
pub trait SignIn: Send + Sync {
    /// The browser was opened to sign in to AWS; the page shows `code`,
    /// which should match.
    fn show_code(&self, code: &str, url: &str);
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

fn config_path() -> PathBuf {
    std::env::var_os("AWS_CONFIG_FILE").map_or_else(|| home().join(".aws/config"), PathBuf::from)
}

fn credentials_path() -> PathBuf {
    std::env::var_os("AWS_SHARED_CREDENTIALS_FILE").map_or_else(|| home().join(".aws/credentials"), PathBuf::from)
}

type Sections = BTreeMap<String, BTreeMap<String, String>>;

/// An INI file's sections and their keys (nested settings, indented under
/// a key, are left out).
pub fn parse_ini(text: &str) -> Sections {
    let mut sections = Sections::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let indented = line.starts_with([' ', '\t']);
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
            sections.entry(name.clone()).or_default();
            current = Some(name);
            continue;
        }
        if indented {
            continue;
        }
        if let (Some(section), Some((key, value))) = (&current, line.split_once('=')) {
            let value = value.trim();
            // An empty value starts nested settings.
            if !value.is_empty() {
                sections.entry(section.clone()).or_default().insert(key.trim().to_lowercase(), value.to_string());
            }
        }
    }
    sections
}

fn read_ini(path: &PathBuf) -> Sections {
    std::fs::read_to_string(path).map(|t| parse_ini(&t)).unwrap_or_default()
}

/// The profiles in the config and credentials files, by name ("default"
/// first).
pub fn profiles() -> Vec<String> {
    let mut names: Vec<String> = read_ini(&config_path())
        .into_keys()
        .filter_map(|s| if s == "default" { Some(s) } else { s.strip_prefix("profile ").map(str::to_string) })
        .collect();
    names.extend(read_ini(&credentials_path()).into_keys());
    names.sort_by_key(|n| (n != "default", n.to_lowercase()));
    names.dedup();
    names
}

/// A profile's settings: its config section, with its credentials section
/// over it.
fn profile_in(config: &Sections, credentials: &Sections, name: &str) -> Option<BTreeMap<String, String>> {
    let section = if name == "default" { "default".to_string() } else { format!("profile {name}") };
    let mut values = config.get(&section).cloned();
    if let Some(keys) = credentials.get(name) {
        values.get_or_insert_with(BTreeMap::new).extend(keys.clone());
    }
    values
}

fn not_found(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("There is no AWS profile \u{201c}{name}\u{201d} in ~/.aws/config or ~/.aws/credentials."),
    )
}

/// The region a profile sets.
pub fn region(name: &str) -> Option<String> {
    profile_in(&read_ini(&config_path()), &read_ini(&credentials_path()), name)?
        .get("region")
        .filter(|r| !r.is_empty())
        .cloned()
}

/// The service addresses (changed only by tests).
#[derive(Clone)]
struct Endpoints {
    oidc: Option<String>,
    portal: Option<String>,
    sts: Option<String>,
}

impl Endpoints {
    fn get() -> Endpoints {
        // Debug builds only: tests point these at a stand-in.
        let var = |name: &str| if cfg!(debug_assertions) { std::env::var(name).ok() } else { None };
        Endpoints {
            oidc: var("FORKZ_TEST_AWS_OIDC"),
            portal: var("FORKZ_TEST_AWS_PORTAL"),
            sts: var("FORKZ_TEST_AWS_STS"),
        }
    }

    fn oidc(&self, region: &str) -> String {
        self.oidc.clone().unwrap_or_else(|| format!("https://oidc.{region}.amazonaws.com"))
    }

    fn portal(&self, region: &str) -> String {
        self.portal.clone().unwrap_or_else(|| format!("https://portal.sso.{region}.amazonaws.com"))
    }

    fn sts(&self) -> String {
        self.sts.clone().unwrap_or_else(|| "https://sts.amazonaws.com".into())
    }
}

/// The keys a profile gives (signing in or asking as needed).
pub fn credentials(name: &str, prompter: &dyn Prompter, sign_in: &dyn SignIn) -> io::Result<Credentials> {
    let config = read_ini(&config_path());
    let keys = read_ini(&credentials_path());
    let agent = http::agent(Duration::from_secs(30));
    resolve(&config, &keys, name, prompter, sign_in, &agent, &Endpoints::get(), 0)
}

#[allow(clippy::too_many_arguments)]
fn resolve(
    config: &Sections,
    keys: &Sections,
    name: &str,
    prompter: &dyn Prompter,
    sign_in: &dyn SignIn,
    agent: &http::Agent,
    endpoints: &Endpoints,
    depth: usize,
) -> io::Result<Credentials> {
    if depth > 5 {
        return Err(io::Error::other("The AWS profiles refer to each other in a loop (source_profile)."));
    }
    let profile = profile_in(config, keys, name).ok_or_else(|| not_found(name))?;
    let get = |key: &str| profile.get(key).filter(|v| !v.is_empty()).cloned();
    if let Some(role) = get("role_arn") {
        let source = match get("source_profile") {
            Some(source) if source == name => static_keys(&profile, name)?,
            Some(source) => resolve(config, keys, &source, prompter, sign_in, agent, endpoints, depth + 1)?,
            None => {
                return Err(io::Error::other(format!(
                    "The AWS profile \u{201c}{name}\u{201d} assumes a role without a source_profile (credential_source is for AWS's own machines)."
                )));
            }
        };
        return assume_role(agent, endpoints, &source, &role, &profile, prompter);
    }
    if get("sso_session").is_some() || get("sso_start_url").is_some() {
        return sso(config, &profile, name, sign_in, agent, endpoints);
    }
    if let Some(command) = get("credential_process") {
        return credential_process(&command);
    }
    static_keys(&profile, name)
}

fn static_keys(profile: &BTreeMap<String, String>, name: &str) -> io::Result<Credentials> {
    let get = |key: &str| profile.get(key).filter(|v| !v.is_empty()).cloned();
    match (get("aws_access_key_id"), get("aws_secret_access_key")) {
        (Some(access_key), Some(secret_key)) => {
            Ok(Credentials { access_key, secret_key, session_token: get("aws_session_token"), expires: None })
        }
        _ => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("The AWS profile \u{201c}{name}\u{201d} has no keys, SSO, role, or credential_process."),
        )),
    }
}

/// Keys from a JSON object with AccessKeyId, SecretAccessKey, and maybe
/// SessionToken and Expiration (credential_process and STS write these).
fn keys_from(value: &Value) -> io::Result<Credentials> {
    let text = |key: &str| value[key].as_str().map(str::to_string);
    let (Some(access_key), Some(secret_key)) = (text("AccessKeyId"), text("SecretAccessKey")) else {
        return Err(io::Error::other("The answer had no AccessKeyId and SecretAccessKey."));
    };
    let expires = text("Expiration").and_then(|t| http::parse_iso(&t));
    Ok(Credentials { access_key, secret_key, session_token: text("SessionToken"), expires })
}

fn credential_process(command: &str) -> io::Result<Credentials> {
    let output = std::process::Command::new("/bin/sh").arg("-c").arg(command).output()?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(io::Error::other(format!("credential_process failed: {error}")));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| io::Error::other(format!("credential_process did not write JSON: {e}")))?;
    keys_from(&value)
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Signature Version 4 headers for a request with a body (any service).
#[allow(clippy::too_many_arguments)]
pub fn sign(
    credentials: &Credentials,
    service: &str,
    region: &str,
    method: &str,
    host: &str,
    path: &str,
    body: &[u8],
    content_type: &str,
    now: SystemTime,
) -> Vec<(String, String)> {
    let (date, stamp) = http::utc_stamp(now);
    let payload = hex(&Sha256::digest(body));
    let mut headers: Vec<(String, String)> = vec![
        ("content-type".into(), content_type.into()),
        ("host".into(), host.into()),
        ("x-amz-date".into(), stamp.clone()),
    ];
    if let Some(token) = &credentials.session_token {
        headers.push(("x-amz-security-token".into(), token.clone()));
    }
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let request = format!("{method}\n{path}\n\n{canonical_headers}\n{signed}\n{payload}");
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let to_sign = format!("AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}", hex(&Sha256::digest(request.as_bytes())));
    let key = hmac(
        &hmac(&hmac(&hmac(format!("AWS4{}", credentials.secret_key).as_bytes(), &date), region), service),
        "aws4_request",
    );
    let signature = hex(&hmac(&key, &to_sign));
    headers.retain(|(k, _)| k != "host");
    headers.push((
        "authorization".into(),
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
            credentials.access_key
        ),
    ));
    headers
}

/// The text of the first element with this name.
fn xml_text(doc: &roxmltree::Document, name: &str) -> Option<String> {
    doc.descendants().find(|n| n.has_tag_name(name)).and_then(|n| n.text()).map(str::to_string)
}

fn assume_role(
    agent: &http::Agent,
    endpoints: &Endpoints,
    source: &Credentials,
    role: &str,
    profile: &BTreeMap<String, String>,
    prompter: &dyn Prompter,
) -> io::Result<Credentials> {
    let get = |key: &str| profile.get(key).filter(|v| !v.is_empty()).cloned();
    let session = get("role_session_name").unwrap_or_else(|| {
        format!("forkz-{}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
    });
    let mut form: Vec<(&str, String)> = vec![
        ("Action", "AssumeRole".into()),
        ("Version", "2011-06-15".into()),
        ("RoleArn", role.into()),
        ("RoleSessionName", session),
        ("DurationSeconds", get("duration_seconds").unwrap_or_else(|| "3600".into())),
    ];
    if let Some(external) = get("external_id") {
        form.push(("ExternalId", external));
    }
    if let Some(serial) = get("mfa_serial") {
        let code = prompter
            .secret(&format!("Enter the MFA code for {serial}"), "The code your authenticator app shows now.")
            .ok_or_else(|| io::Error::new(io::ErrorKind::Interrupted, "Cancelled"))?;
        form.push(("SerialNumber", serial));
        form.push(("TokenCode", code.trim().to_string()));
    }
    let body: String = form.iter().map(|(k, v)| format!("{k}={}", http::encode_value(v))).collect::<Vec<_>>().join("&");
    let url = endpoints.sts();
    let host = url.split("://").nth(1).unwrap_or(&url).trim_end_matches('/').to_string();
    let content_type = "application/x-www-form-urlencoded; charset=utf-8";
    let headers =
        sign(source, "sts", "us-east-1", "POST", &host, "/", body.as_bytes(), content_type, SystemTime::now());
    let mut request = ureq::http::Request::builder().method("POST").uri(format!("{}/", url.trim_end_matches('/')));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = agent.run(request.body(body.into_bytes()).map_err(io::Error::other)?).map_err(http::to_io)?;
    let status = response.status().as_u16();
    let text = response.into_body().read_to_string().map_err(http::to_io)?;
    let doc = roxmltree::Document::parse(&text).map_err(io::Error::other)?;
    if !(200..300).contains(&status) {
        let message = xml_text(&doc, "Message").unwrap_or(text.clone());
        let kind = if status == 401 || status == 403 { io::ErrorKind::PermissionDenied } else { io::ErrorKind::Other };
        return Err(io::Error::new(kind, format!("The role could not be assumed: {}", message.trim())));
    }
    let value = json!({
        "AccessKeyId": xml_text(&doc, "AccessKeyId"),
        "SecretAccessKey": xml_text(&doc, "SecretAccessKey"),
        "SessionToken": xml_text(&doc, "SessionToken"),
        "Expiration": xml_text(&doc, "Expiration"),
    });
    keys_from(&value)
}

// MARK: IAM Identity Center (SSO)

/// Where the sign-in for a session (or a start URL, in older profiles)
/// is kept: the AWS CLI's cache.
fn cache_file(key: &str) -> PathBuf {
    use sha1::Sha1;
    let name = hex(&Sha1::digest(key.as_bytes()));
    home().join(".aws/sso/cache").join(format!("{name}.json"))
}

fn post_json(agent: &http::Agent, url: &str, body: &Value) -> Result<Value, (u16, Value)> {
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    let request = ureq::http::Request::builder()
        .method("POST")
        .uri(url)
        .header("Content-Type", "application/json")
        .body(bytes)
        .map_err(|e| (0, json!({ "error_description": e.to_string() })))?;
    let response = agent.run(request).map_err(|e| (0, json!({ "error_description": e.to_string() })))?;
    let status = response.status().as_u16();
    let text = response.into_body().read_to_string().unwrap_or_default();
    let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if (200..300).contains(&status) { Ok(value) } else { Err((status, value)) }
}

fn sso_error(what: &str, (status, value): (u16, Value)) -> io::Error {
    let detail = value["error_description"].as_str().or(value["message"].as_str()).or(value["error"].as_str());
    io::Error::new(
        if status == 401 || status == 403 { io::ErrorKind::PermissionDenied } else { io::ErrorKind::Other },
        format!("{what}: {}", detail.map_or_else(|| format!("AWS answered {status}"), str::to_string)),
    )
}

fn later(seconds: u64) -> String {
    http::iso_utc(SystemTime::now() + Duration::from_secs(seconds))
}

fn sso(
    config: &Sections,
    profile: &BTreeMap<String, String>,
    name: &str,
    sign_in: &dyn SignIn,
    agent: &http::Agent,
    endpoints: &Endpoints,
) -> io::Result<Credentials> {
    let get = |key: &str| profile.get(key).filter(|v| !v.is_empty()).cloned();
    let missing =
        |key: &str| io::Error::other(format!("The AWS profile \u{201c}{name}\u{201d} uses SSO but has no {key}."));
    // A named session ([sso-session NAME]), or the older settings in the
    // profile itself.
    let (cache_key, start_url, sso_region, scopes) = match get("sso_session") {
        Some(session) => {
            let section = config
                .get(&format!("sso-session {session}"))
                .ok_or_else(|| io::Error::other(format!("There is no [sso-session {session}] in ~/.aws/config.")))?;
            let value = |key: &str| section.get(key).filter(|v| !v.is_empty()).cloned();
            let scopes: Vec<String> = value("sso_registration_scopes")
                .unwrap_or_else(|| "sso:account:access".into())
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            (
                session.clone(),
                value("sso_start_url").ok_or_else(|| missing("sso_start_url"))?,
                value("sso_region").ok_or_else(|| missing("sso_region"))?,
                scopes,
            )
        }
        None => {
            let url = get("sso_start_url").ok_or_else(|| missing("sso_start_url"))?;
            (url.clone(), url, get("sso_region").ok_or_else(|| missing("sso_region"))?, Vec::new())
        }
    };
    let account = get("sso_account_id").ok_or_else(|| missing("sso_account_id"))?;
    let role = get("sso_role_name").ok_or_else(|| missing("sso_role_name"))?;
    let path = cache_file(&cache_key);
    let mut token = sso_token(agent, endpoints, &path, &start_url, &sso_region, &scopes, sign_in, false)?;
    for attempt in 0..2 {
        let url = format!(
            "{}/federation/credentials?account_id={}&role_name={}",
            endpoints.portal(&sso_region),
            http::encode_value(&account),
            http::encode_value(&role)
        );
        let request = ureq::http::Request::builder()
            .method("GET")
            .uri(url)
            .header("x-amz-sso_bearer_token", token.as_str())
            .body(())
            .map_err(io::Error::other)?;
        let response = agent.run(request).map_err(http::to_io)?;
        let status = response.status().as_u16();
        let text = response.into_body().read_to_string().unwrap_or_default();
        // The sign-in was withdrawn: sign in again, once.
        if status == 401 && attempt == 0 {
            token = sso_token(agent, endpoints, &path, &start_url, &sso_region, &scopes, sign_in, true)?;
            continue;
        }
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            return Err(sso_error(&format!("The role {role} in account {account} could not be used"), (status, value)));
        }
        let credentials = &value["roleCredentials"];
        let text = |key: &str| credentials[key].as_str().map(str::to_string);
        let (Some(access_key), Some(secret_key)) = (text("accessKeyId"), text("secretAccessKey")) else {
            return Err(io::Error::other("AWS answered without keys."));
        };
        let expires = credentials["expiration"].as_u64().map(|ms| UNIX_EPOCH + Duration::from_millis(ms));
        return Ok(Credentials { access_key, secret_key, session_token: text("sessionToken"), expires });
    }
    Err(io::Error::new(io::ErrorKind::PermissionDenied, "The SSO sign-in was not accepted."))
}

/// An SSO access token: the cached one while it lasts, renewed if it can
/// be, else from signing in in the browser. `fresh`: sign in again.
#[allow(clippy::too_many_arguments)]
fn sso_token(
    agent: &http::Agent,
    endpoints: &Endpoints,
    path: &PathBuf,
    start_url: &str,
    region: &str,
    scopes: &[String],
    sign_in: &dyn SignIn,
    fresh: bool,
) -> io::Result<String> {
    let mut cache: Value =
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}));
    let text = |cache: &Value, key: &str| cache[key].as_str().map(str::to_string);
    let valid = |cache: &Value, key: &str| {
        text(cache, key)
            .and_then(|t| http::parse_iso(&t))
            .is_some_and(|t| t > SystemTime::now() + Duration::from_secs(60))
    };
    if !fresh
        && valid(&cache, "expiresAt")
        && let Some(token) = text(&cache, "accessToken")
    {
        return Ok(token);
    }
    let oidc = endpoints.oidc(region);
    // Renew with the refresh token, if there is one.
    if !fresh
        && valid(&cache, "registrationExpiresAt")
        && let (Some(client), Some(secret), Some(refresh)) =
            (text(&cache, "clientId"), text(&cache, "clientSecret"), text(&cache, "refreshToken"))
    {
        let body = json!({ "clientId": client, "clientSecret": secret, "grantType": "refresh_token", "refreshToken": refresh });
        if let Ok(answer) = post_json(agent, &format!("{oidc}/token"), &body) {
            return save_token(path, &mut cache, &answer);
        }
    }
    // Sign in: register forkz (or reuse its registration), then the
    // device sign-in in the browser.
    if !valid(&cache, "registrationExpiresAt") || text(&cache, "clientId").is_none() {
        let mut body = json!({ "clientName": "forkz", "clientType": "public" });
        if !scopes.is_empty() {
            body["scopes"] = json!(scopes);
            body["grantTypes"] = json!(["urn:ietf:params:oauth:grant-type:device_code", "refresh_token"]);
        }
        let answer = post_json(agent, &format!("{oidc}/client/register"), &body)
            .map_err(|e| sso_error("forkz could not register for the SSO sign-in", e))?;
        cache["clientId"] = answer["clientId"].clone();
        cache["clientSecret"] = answer["clientSecret"].clone();
        if let Some(until) = answer["clientSecretExpiresAt"].as_u64() {
            cache["registrationExpiresAt"] = json!(http::iso_utc(UNIX_EPOCH + Duration::from_secs(until)));
        }
    }
    let client = text(&cache, "clientId").unwrap_or_default();
    let secret = text(&cache, "clientSecret").unwrap_or_default();
    let body = json!({ "clientId": client, "clientSecret": secret, "startUrl": start_url });
    let device = post_json(agent, &format!("{oidc}/device_authorization"), &body)
        .map_err(|e| sso_error("The SSO sign-in could not start", e))?;
    let device_code = device["deviceCode"].as_str().unwrap_or_default().to_string();
    let url = device["verificationUriComplete"].as_str().or(device["verificationUri"].as_str()).unwrap_or_default();
    let code = device["userCode"].as_str().unwrap_or_default();
    let mut interval = Duration::from_secs(device["interval"].as_u64().unwrap_or(5).max(1));
    let deadline = Instant::now() + Duration::from_secs(device["expiresIn"].as_u64().unwrap_or(600));
    // The browser is not opened by tests (the stand-in has no page).
    if !cfg!(debug_assertions) || std::env::var_os("FORKZ_TEST_AWS_OIDC").is_none() {
        std::process::Command::new("/usr/bin/open").arg(url).status()?;
    }
    sign_in.show_code(code, url);
    let body = json!({
        "clientId": client,
        "clientSecret": secret,
        "grantType": "urn:ietf:params:oauth:grant-type:device_code",
        "deviceCode": device_code,
    });
    while Instant::now() < deadline {
        std::thread::sleep(interval);
        match post_json(agent, &format!("{oidc}/token"), &body) {
            Ok(answer) => {
                cache["startUrl"] = json!(start_url);
                cache["region"] = json!(region);
                return save_token(path, &mut cache, &answer);
            }
            Err((_, value)) if value["error"] == "authorization_pending" => {}
            Err((_, value)) if value["error"] == "slow_down" => interval += Duration::from_secs(5),
            Err(e) => return Err(sso_error("The SSO sign-in did not finish", e)),
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "The SSO sign-in was not finished in time."))
}

/// Keeps a new token in the cache (as the AWS CLI does); returns it.
fn save_token(path: &PathBuf, cache: &mut Value, answer: &Value) -> io::Result<String> {
    let token = answer["accessToken"].as_str().ok_or_else(|| io::Error::other("AWS answered without a token."))?;
    cache["accessToken"] = json!(token);
    cache["expiresAt"] = json!(later(answer["expiresIn"].as_u64().unwrap_or(3600)));
    if let Some(refresh) = answer["refreshToken"].as_str() {
        cache["refreshToken"] = json!(refresh);
    }
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    // Only the user may read it (as the CLI keeps it).
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    io::Write::write_all(&mut file, serde_json::to_string_pretty(cache).map_err(io::Error::other)?.as_bytes())?;
    Ok(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_profiles() {
        let config = parse_ini(
            "[default]\nregion = eu-west-1\n\n[profile work]\nsso_session = corp\nsso_account_id = 111122223333\nsso_role_name = Dev\nregion = us-west-2\ns3 =\n  max_concurrent_requests = 20\n\n[sso-session corp]\nsso_start_url = https://corp.awsapps.com/start\nsso_region = us-east-1\n# comment\n[profile  admin]\nrole_arn = arn:aws:iam::1:role/Admin\nsource_profile = default\n",
        );
        let keys = parse_ini(
            "[default]\naws_access_key_id = AKIAEXAMPLE\naws_secret_access_key = secret\n[other]\naws_access_key_id=A\naws_secret_access_key=B\naws_session_token=T\n",
        );
        let work = profile_in(&config, &keys, "work").unwrap();
        assert_eq!(work.get("region").map(String::as_str), Some("us-west-2"));
        assert!(!work.contains_key("s3"), "nested settings are left out");
        assert!(!work.contains_key("max_concurrent_requests"));
        assert!(profile_in(&config, &keys, "admin").is_some(), "spaces in section names");
        let default = profile_in(&config, &keys, "default").unwrap();
        assert_eq!(static_keys(&default, "default").unwrap().access_key, "AKIAEXAMPLE");
        let other = static_keys(&profile_in(&config, &keys, "other").unwrap(), "other").unwrap();
        assert_eq!(other.session_token.as_deref(), Some("T"));
        assert!(profile_in(&config, &keys, "missing").is_none());
        // The CLI's cache file names: SHA-1 of the session name.
        assert!(cache_file("corp").ends_with(".aws/sso/cache/ee0bfd2552fbd840c02cc48b6e823320543c450f.json"));
    }

    #[test]
    fn reads_process_keys() {
        let keys = credential_process(
            r#"echo '{"Version":1,"AccessKeyId":"AKIA1","SecretAccessKey":"s","SessionToken":"t","Expiration":"2030-01-01T00:00:00Z"}'"#,
        )
        .unwrap();
        assert_eq!(keys.access_key, "AKIA1");
        assert_eq!(keys.session_token.as_deref(), Some("t"));
        assert!(!keys.expiring(Duration::from_secs(60)));
        assert!(credential_process("exit 3").is_err());
        assert!(credential_process("echo nope").is_err());
        // Secrets stay out of logs.
        assert!(!format!("{keys:?}").contains("\"s\""));
    }

    /// The "post-x-www-form-urlencoded" case of AWS's Signature Version 4
    /// test suite.
    #[test]
    fn signs_like_aws() {
        let credentials = Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
            expires: None,
        };
        let now = UNIX_EPOCH + Duration::from_secs(1_440_938_160); // 20150830T123600Z
        let headers = sign(
            &credentials,
            "service",
            "us-east-1",
            "POST",
            "example.amazonaws.com",
            "/",
            b"Param1=value1",
            "application/x-www-form-urlencoded",
            now,
        );
        let authorization = headers.iter().find(|(k, _)| k == "authorization").unwrap().1.clone();
        assert_eq!(
            authorization,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=content-type;host;x-amz-date, Signature=ff11897932ad3f4e8b18135d722051e5ac45fc38421b1da7b9d196a0fe09473a"
        );
    }
}
