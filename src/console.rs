use crate::alarms::AlarmStore;
use crate::audit;
use crate::config::VoicePrefs;
use crate::lock::ConsoleGuard;
use crate::tools::ToolHost;
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{from_fn_with_state, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Clone)]
pub struct ConsoleState {
    pub host: ToolHost,
    pub alarms: AlarmStore,
    pub guard: ConsoleGuard,
}

pub fn router(state: ConsoleState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/live", get(index))
        .route("/activity", get(index))
        .route("/house", get(index))
        .route("/music", get(index))
        .route("/alarms", get(index))
        .route("/timers", get(index))
        .route("/settings", get(index))
        .route("/health", get(health))
        .route("/api/settings", get(read_settings).put(write_settings))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/lock", get(lock_status).post(lock_change))
        .route("/api/confirm", post(confirm))
        .route("/api/timers", post(add_timer))
        .route("/api/timers/cancel", post(cancel_timer))
        .route("/api/audit", get(audit_tail))
        .route("/api/aliases", get(read_aliases).put(write_aliases))
        .route("/api/lights", get(lights).post(set_light))
        .route("/api/music", get(music).post(music_action))
        .route("/api/alarms", get(alarms).post(add_alarm))
        .route("/api/alarms/cancel", post(cancel_alarm))
        .route("/api/alarms/enabled", post(enable_alarm))
        .route("/api/security", post(security))
        .route("/api/doorbell.jpg", get(doorbell))
        .route_layer(from_fn_with_state(state.clone(), gate))
        .with_state(state)
}

async fn gate(State(state): State<ConsoleState>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_string();
    if is_public(&path) || state.guard.allows(cookie_token(request.headers()).as_deref()) {
        return next.run(request).await;
    }
    (StatusCode::UNAUTHORIZED, "sign in").into_response()
}

fn is_public(path: &str) -> bool {
    matches!(
        path,
        "/" | "/live"
            | "/activity"
            | "/house"
            | "/music"
            | "/alarms"
            | "/timers"
            | "/settings"
            | "/api/login"
            | "/api/lock"
    )
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        let Some(value) = part.strip_prefix("grok_console=") else {
            continue;
        };
        let token = value.trim();
        if token.len() == 64 && token.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Some(token.to_string());
        }
    }
    None
}

fn session_cookie(token: &str, max_age: u64) -> String {
    format!("grok_console={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={max_age}")
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../assets/dashboard.html"))
}

async fn health(State(state): State<ConsoleState>) -> Json<Value> {
    state.host.app.set_room(&state.host.settings.live().room);
    state.host.app.refresh_quiet(&state.host.settings);
    let timers = state
        .host
        .timers
        .list()
        .into_iter()
        .map(|timer| {
            json!({
                "id": timer.id,
                "label": timer.label,
                "fire_at_unix": timer.fire_at_unix,
            })
        })
        .collect();
    let mut body = state.host.app.snapshot(timers);
    if let Some(object) = body.as_object_mut() {
        object.insert("ha_configured".into(), json!(state.host.ha.is_some()));
        object.insert("alarms".into(), json!(state.alarms.list()));
    }
    Json(body)
}

async fn read_settings(State(state): State<ConsoleState>) -> Json<VoicePrefs> {
    Json(state.host.settings.live())
}

async fn write_settings(
    State(state): State<ConsoleState>,
    Json(body): Json<Value>,
) -> Result<Json<VoicePrefs>, (StatusCode, String)> {
    match state.host.settings.apply_json(&body) {
        Ok(prefs) => {
            state.host.app.set_room(&prefs.room);
            state.host.app.refresh_quiet(&state.host.settings);
            Ok(Json(prefs))
        }
        Err(err) => Err((StatusCode::BAD_REQUEST, err)),
    }
}

#[derive(Deserialize)]
struct LoginBody {
    password: String,
}

async fn login(
    State(state): State<ConsoleState>,
    Json(body): Json<LoginBody>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    match state.guard.login(&body.password) {
        Ok(token) => Ok((
            [(header::SET_COOKIE, session_cookie(&token, 1_209_600))],
            Json(json!({ "ok": true })),
        )
            .into_response()),
        Err(message) => {
            let code = if message.contains("wait") {
                StatusCode::TOO_MANY_REQUESTS
            } else if message.contains("wrong") {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::BAD_REQUEST
            };
            Err((code, Json(json!({ "ok": false, "error": message }))))
        }
    }
}

async fn logout(State(state): State<ConsoleState>, request: Request) -> Response {
    if let Some(token) = cookie_token(request.headers()) {
        state.guard.logout(&token);
    }
    ([(header::SET_COOKIE, session_cookie("", 0))], Json(json!({ "ok": true }))).into_response()
}

async fn lock_status(State(state): State<ConsoleState>) -> Json<Value> {
    let (required, from_env) = state.guard.status();
    Json(json!({ "required": required, "from_env": from_env }))
}

#[derive(Deserialize)]
struct LockBody {
    current: Option<String>,
    password: Option<String>,
    clear: Option<bool>,
}

async fn lock_change(
    State(state): State<ConsoleState>,
    Json(body): Json<LockBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let result = if body.clear.unwrap_or(false) {
        state
            .guard
            .clear_password(body.current.as_deref().unwrap_or(""))
    } else {
        state
            .guard
            .set_password(body.current.as_deref(), body.password.as_deref().unwrap_or(""))
    };
    match result {
        Ok(()) => Ok(Json(json!({ "ok": true, "signed_out": true }))),
        Err(err) => Err((StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": err })))),
    }
}

#[derive(Deserialize)]
struct YesBody {
    yes: bool,
}

async fn confirm(State(state): State<ConsoleState>, Json(body): Json<YesBody>) -> impl IntoResponse {
    json_result(state.host.answer_confirmation(body.yes).await)
}

#[derive(Deserialize)]
struct TimerBody {
    label: Option<String>,
    seconds: Option<u64>,
}

async fn add_timer(State(state): State<ConsoleState>, Json(body): Json<TimerBody>) -> impl IntoResponse {
    json_result(
        state
            .host
            .dashboard_add_timer(body.seconds.unwrap_or(0), body.label.as_deref().unwrap_or("timer")),
    )
}

#[derive(Deserialize)]
struct IdBody {
    id: u64,
}

async fn cancel_timer(State(state): State<ConsoleState>, Json(body): Json<IdBody>) -> impl IntoResponse {
    json_result(state.host.dashboard_cancel_timer(body.id))
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

async fn audit_tail(State(state): State<ConsoleState>, Query(query): Query<LimitQuery>) -> Json<Value> {
    let limit = query.limit.unwrap_or(40).clamp(1, 80);
    let path = state.host.settings.log_dir.join("audit.jsonl");
    let lines: Vec<Value> = audit::tail(&path, limit).into_iter().map(scrub_audit).collect();
    Json(json!({ "lines": lines }))
}

fn scrub_audit(mut value: Value) -> Value {
    if let Some(args) = value.get_mut("args").and_then(|item| item.as_object_mut()) {
        args.remove("code");
        args.remove("password");
        args.remove("token");
    }
    value
}

async fn read_aliases(State(state): State<ConsoleState>) -> Json<HashMap<String, String>> {
    Json(state.host.alias_map())
}

async fn write_aliases(
    State(state): State<ConsoleState>,
    Json(body): Json<HashMap<String, String>>,
) -> Result<Json<HashMap<String, String>>, (StatusCode, Json<Value>)> {
    match state.host.replace_aliases(body) {
        Ok(map) => Ok(Json(map)),
        Err(err) => Err((StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": err })))),
    }
}

async fn lights(State(state): State<ConsoleState>) -> impl IntoResponse {
    json_result(state.host.dashboard_lights().await)
}

#[derive(Deserialize)]
struct LightBody {
    entity_id: String,
    on: bool,
}

async fn set_light(State(state): State<ConsoleState>, Json(body): Json<LightBody>) -> impl IntoResponse {
    json_result(state.host.dashboard_set_light(&body.entity_id, body.on).await)
}

async fn music(State(state): State<ConsoleState>) -> impl IntoResponse {
    json_result(state.host.dashboard_players().await)
}

#[derive(Deserialize)]
struct MusicBody {
    entity_id: String,
    action: String,
    volume: Option<f64>,
    source: Option<String>,
    query: Option<String>,
}

async fn music_action(State(state): State<ConsoleState>, Json(body): Json<MusicBody>) -> impl IntoResponse {
    json_result(
        state
            .host
            .dashboard_media(
                &body.entity_id,
                &body.action,
                body.volume,
                body.source.as_deref(),
                body.query.as_deref(),
            )
            .await,
    )
}

async fn alarms(State(state): State<ConsoleState>) -> Json<Value> {
    let panels = state.host.dashboard_security().await;
    Json(json!({
        "clock": state.alarms.list(),
        "panels": panels.get("panels").cloned().unwrap_or_else(|| json!([])),
        "ok": panels.get("ok").cloned().unwrap_or(json!(false)),
        "error": panels.get("error").cloned(),
    }))
}

#[derive(Deserialize)]
struct AlarmBody {
    label: Option<String>,
    hour: u8,
    minute: u8,
    days: Option<Vec<u8>>,
}

async fn add_alarm(
    State(state): State<ConsoleState>,
    Json(body): Json<AlarmBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match state.alarms.add(
        body.label.as_deref().unwrap_or("Alarm"),
        body.hour,
        body.minute,
        body.days.unwrap_or_default(),
    ) {
        Ok(alarm) => Ok(Json(json!({ "ok": true, "alarm": alarm }))),
        Err(err) => Err((StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": err })))),
    }
}

async fn cancel_alarm(
    State(state): State<ConsoleState>,
    Json(body): Json<IdBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match state.alarms.cancel(body.id) {
        Some(alarm) => Ok(Json(json!({ "ok": true, "alarm": alarm }))),
        None => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "no matching alarm" })),
        )),
    }
}

#[derive(Deserialize)]
struct EnableBody {
    id: u64,
    enabled: bool,
}

async fn enable_alarm(
    State(state): State<ConsoleState>,
    Json(body): Json<EnableBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match state.alarms.set_enabled(body.id, body.enabled) {
        Some(alarm) => Ok(Json(json!({ "ok": true, "alarm": alarm }))),
        None => Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "ok": false, "error": "no matching alarm" })),
        )),
    }
}

#[derive(Deserialize)]
struct SecurityBody {
    entity_id: String,
    service: String,
    code: Option<String>,
}

async fn security(State(state): State<ConsoleState>, Json(body): Json<SecurityBody>) -> impl IntoResponse {
    json_result(
        state
            .host
            .dashboard_arm(&body.entity_id, &body.service, body.code.as_deref())
            .await,
    )
}

async fn doorbell(State(state): State<ConsoleState>) -> Response {
    let path = state.host.settings.data_dir.join("last-doorbell.jpg");
    match tokio::fs::read(path).await {
        Ok(bytes) => Response::builder()
            .header(header::CONTENT_TYPE, "image/jpeg")
            .header(header::CACHE_CONTROL, "no-store")
            .body(axum::body::Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::NOT_FOUND.into_response()),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn json_result(value: Value) -> (StatusCode, Json<Value>) {
    let needs = value.get("needs_confirmation").and_then(|item| item.as_bool()) == Some(true);
    let ok = value.get("ok").and_then(|item| item.as_bool()).unwrap_or(true);
    let code = if ok || needs {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };
    (code, Json(value))
}
