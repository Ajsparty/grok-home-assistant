use crate::audit::AuditLog;
use crate::config::Settings;
use crate::fast_path::{FastAction, MediaAction};
use crate::ha::{self, first_domain, trim_entity, HaClient, HaEntity};
use crate::state::AppState;
use crate::textutil::normalize;
use crate::timers::{Timer, TimerStore};
use crate::vision;
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TURN_DOMAINS: &[&str] = &[
    "light",
    "switch",
    "fan",
    "input_boolean",
    "humidifier",
    "media_player",
];

const ALLOWED_DOMAINS: &[&str] = &[
    "light",
    "switch",
    "fan",
    "climate",
    "cover",
    "media_player",
    "todo",
    "scene",
    "script",
    "input_boolean",
    "input_number",
    "vacuum",
    "humidifier",
    "remote",
    "button",
    "number",
    "select",
    "lock",
    "alarm_control_panel",
];

pub fn load_aliases(path: &Path) -> HashMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let Ok(raw) = serde_json::from_str::<HashMap<String, String>>(&text) else {
        log::warn!("could not parse aliases in {}", path.display());
        return HashMap::new();
    };
    raw.into_iter()
        .map(|(key, value)| (normalize(&key), value))
        .collect()
}

#[derive(Clone)]
pub struct ToolHost {
    pub settings: Settings,
    pub app: AppState,
    pub ha: Option<HaClient>,
    pub timers: TimerStore,
    pub audit: AuditLog,
    pub aliases: Arc<Mutex<HashMap<String, String>>>,
    recent: Arc<Mutex<Vec<(String, Instant)>>>,
    pending: Arc<Mutex<Option<Pending>>>,
}

struct Pending {
    domain: String,
    service: String,
    entity_id: String,
    data: Value,
    summary: String,
    expires: Instant,
}

impl ToolHost {
    pub fn new(
        settings: Settings,
        app: AppState,
        ha: Option<HaClient>,
        timers: TimerStore,
        audit: AuditLog,
        aliases: HashMap<String, String>,
    ) -> Self {
        Self {
            settings,
            app,
            ha,
            timers,
            audit,
            aliases: Arc::new(Mutex::new(aliases)),
            recent: Arc::new(Mutex::new(Vec::new())),
            pending: Arc::new(Mutex::new(None)),
        }
    }

    pub fn execute<'a>(
        &'a self,
        name: &'a str,
        args: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            let result = self.dispatch(name, &args).await;
            self.audit.record(&json!({
                "ts": Utc::now().to_rfc3339(),
                "room": self.settings.live().room,
                "tool": name,
                "args": args,
                "ok": result.get("ok").and_then(|value| value.as_bool()).unwrap_or(true),
                "error": result.get("error"),
            }));
            self.app.set_last_tool(name);
            result
        })
    }

    pub async fn run_fast(&self, action: FastAction) -> Value {
        let result = match action {
            FastAction::Turn { on, target } => self.turn_named(on, &target).await,
            FastAction::Media(action) => {
                let name = match &action {
                    MediaAction::Play => "play",
                    MediaAction::Pause => "pause",
                    MediaAction::Next => "next",
                    MediaAction::Previous => "previous",
                    MediaAction::VolumeUp => "volume_up",
                    MediaAction::VolumeDown => "volume_down",
                    MediaAction::SetVolume(_) => "set_volume",
                };
                self.media(name, action).await
            }
            FastAction::Timer { seconds, label } => self.add_timer(seconds, &label),
        };
        self.audit.record(&json!({
            "ts": Utc::now().to_rfc3339(),
            "room": self.settings.live().room,
            "tool": "fast_path",
            "ok": result.get("ok").and_then(|value| value.as_bool()).unwrap_or(false),
            "error": result.get("error"),
        }));
        if result.get("ok").and_then(|value| value.as_bool()) == Some(true) {
            self.app.set_last_tool("fast_path");
        }
        result
    }

    async fn dispatch(&self, name: &str, args: &Value) -> Value {
        match name {
            "search_entities" => self.search(args.get("query").and_then(|v| v.as_str()).unwrap_or("")).await,
            "get_state" => self.get_state(args.get("entity_id").and_then(|v| v.as_str()).unwrap_or("")).await,
            "call_service" => {
                self.call_service(
                    args.get("domain").and_then(|v| v.as_str()).unwrap_or(""),
                    args.get("service").and_then(|v| v.as_str()).unwrap_or(""),
                    args.get("entity_id").and_then(|v| v.as_str()).unwrap_or(""),
                    args.get("data").cloned().unwrap_or_else(|| json!({})),
                    false,
                )
                .await
            }
            "confirm_action" => {
                self.confirm(args.get("confirmed").and_then(|v| v.as_bool()).unwrap_or(false))
                    .await
            }
            "set_timer" => {
                let seconds = args
                    .get("seconds")
                    .and_then(|v| v.as_u64())
                    .or_else(|| args.get("minutes").and_then(|v| v.as_u64()).map(|m| m * 60))
                    .unwrap_or(0);
                let label = args.get("label").and_then(|v| v.as_str()).unwrap_or("timer");
                self.add_timer(seconds, label)
            }
            "cancel_timer" => self.cancel_timer(args),
            "list_timers" => ok(json!({ "timers": self.timer_values() })),
            "media_control" => {
                let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
                let parsed = match action {
                    "play" => MediaAction::Play,
                    "pause" => MediaAction::Pause,
                    "play_pause" => MediaAction::Play,
                    "next" => MediaAction::Next,
                    "previous" => MediaAction::Previous,
                    "volume_up" => MediaAction::VolumeUp,
                    "volume_down" => MediaAction::VolumeDown,
                    "set_volume" => MediaAction::SetVolume(args.get("volume").and_then(|v| v.as_f64()).unwrap_or(0.3) as f32),
                    _ => return fail(format!("unknown media action {action}")),
                };
                if let Some(entity) = args.get("entity_id").and_then(|value| value.as_str()) {
                    if !entity.is_empty() {
                        return self.media_entity(action, parsed, entity).await;
                    }
                }
                self.media(action, parsed).await
            }
            "todo_list" => self.todo(args).await,
            "morning_brief" => self.morning_brief().await,
            "who_is_home" => self.who_is_home().await,
            "describe_camera" => {
                self.describe_camera(args.get("entity_id").and_then(|v| v.as_str()))
                    .await
            }
            "recent_events" => ok(json!({ "events": self.app.recent_events() })),
            _ => fail(format!("unknown tool {name}")),
        }
    }

    fn add_timer(&self, seconds: u64, label: &str) -> Value {
        if seconds == 0 || seconds > 24 * 60 * 60 {
            return fail("timer length must be between 1 second and 24 hours");
        }
        let timer = self.timers.add(label, seconds);
        ok(json!({
            "timer": timer_value(&timer),
            "message": format!("Timer {label} set for {seconds} seconds"),
        }))
    }

    fn cancel_timer(&self, args: &Value) -> Value {
        let id = args.get("id").and_then(|v| v.as_u64());
        let label = args.get("label").and_then(|v| v.as_str());
        match self.timers.cancel(id, label) {
            Some(timer) => ok(json!({ "cancelled": timer_value(&timer) })),
            None => fail("no matching timer"),
        }
    }

    async fn search(&self, query: &str) -> Value {
        let Some(ha) = &self.ha else {
            return fail("home assistant is not configured");
        };
        if let Some(entity_id) = self.alias_lookup(&normalize(query)) {
            return ok(json!({ "matches": [{ "entity_id": entity_id, "alias": query }] }));
        }
        let room = self.settings.live().room;
        match ha.search(query, &room, 8).await {
            Ok(matches) => ok(json!({
                "matches": matches.iter().map(trim_entity).collect::<Vec<_>>()
            })),
            Err(err) => fail(err.to_string()),
        }
    }

    async fn get_state(&self, entity_id: &str) -> Value {
        let Some(ha) = &self.ha else {
            return fail("home assistant is not configured");
        };
        if entity_id.is_empty() {
            return fail("entity_id is required");
        }
        match ha.states(false).await {
            Ok(states) => match states.into_iter().find(|entity| entity.entity_id == entity_id) {
                Some(entity) => ok(trim_entity(&entity)),
                None => fail(format!("no entity {entity_id}")),
            },
            Err(err) => fail(err.to_string()),
        }
    }

    async fn call_service(
        &self,
        domain: &str,
        service: &str,
        entity_id: &str,
        data: Value,
        from_confirm: bool,
    ) -> Value {
        let Some(ha) = &self.ha else {
            return fail("home assistant is not configured");
        };
        if !ALLOWED_DOMAINS.contains(&domain) {
            return fail(format!("{domain} is not on the voice allowlist"));
        }
        if entity_id.is_empty() || !entity_id.contains('.') {
            return fail("entity_id must look like light.kitchen");
        }
        if !from_confirm
            && needs_confirmation(
                domain,
                service,
                entity_id,
                &self.settings.confirm_domains,
                &self.settings.confirm_match,
            )
        {
            let summary = format!("{domain}.{service} on {entity_id}");
            if let Ok(mut slot) = self.pending.lock() {
                *slot = Some(Pending {
                    domain: domain.to_string(),
                    service: service.to_string(),
                    entity_id: entity_id.to_string(),
                    data,
                    summary: summary.clone(),
                    expires: Instant::now() + Duration::from_secs(45),
                });
            }
            self.app.set_pending(Some(summary.clone()));
            return json!({
                "ok": false,
                "needs_confirmation": true,
                "summary": summary,
                "message": "Ask the household if they want this done. Call confirm_action only after they clearly agree.",
            });
        }
        let key = format!("{domain}:{service}:{entity_id}");
        if !from_confirm && self.recently_done(&key) {
            return ok(json!({ "entity_id": entity_id, "message": "already done" }));
        }
        let mut body = if data.is_object() { data } else { json!({}) };
        if let Some(object) = body.as_object_mut() {
            object.insert("entity_id".into(), json!(entity_id));
        }
        match ha.call_service(domain, service, body).await {
            Ok(_) => {
                self.remember(key);
                ok(json!({ "entity_id": entity_id, "service": format!("{domain}.{service}") }))
            }
            Err(err) => fail(err.to_string()),
        }
    }

    async fn confirm(&self, confirmed: bool) -> Value {
        let pending = self.pending.lock().ok().and_then(|mut slot| slot.take());
        self.app.set_pending(None);
        let Some(pending) = pending else {
            return fail("nothing is waiting for confirmation");
        };
        if pending.expires < Instant::now() {
            return fail("the confirmation expired");
        }
        if !confirmed {
            return ok(json!({ "cancelled": pending.summary }));
        }
        self.call_service(
            &pending.domain,
            &pending.service,
            &pending.entity_id,
            pending.data,
            true,
        )
        .await
    }

    async fn turn_named(&self, on: bool, target: &str) -> Value {
        let Some(ha) = &self.ha else {
            return fail("home assistant is not configured");
        };
        let entities = match self.resolve_turn(ha, target).await {
            Ok(entities) => entities,
            Err(message) => return fail(message),
        };
        if entities.is_empty() {
            return json!({"ok": false, "ambiguous": true, "message": "no single device match"});
        }
        let service = if on { "turn_on" } else { "turn_off" };
        let mut done = Vec::new();
        for entity in entities {
            let result = self
                .call_service(&entity.domain, service, &entity.entity_id, json!({}), false)
                .await;
            if result.get("needs_confirmation") == Some(&Value::Bool(true)) {
                return result;
            }
            if result.get("ok") == Some(&Value::Bool(false)) {
                return result;
            }
            done.push(entity.entity_id);
        }
        ok(json!({ "entities": done, "service": service }))
    }

    async fn resolve_turn(&self, ha: &HaClient, target: &str) -> Result<Vec<HaEntity>, String> {
        let query = normalize(target);
        if let Some(entity_id) = self.alias_lookup(&query) {
            let states = ha.states(false).await.map_err(|err| err.to_string())?;
            if let Some(entity) = states.into_iter().find(|entity| entity.entity_id == entity_id) {
                return Ok(vec![entity]);
            }
            return Err(format!("alias {target} points at missing {entity_id}"));
        }
        let states = ha.states(false).await.map_err(|err| err.to_string())?;
        if matches!(query.as_str(), "light" | "lights") {
            let room = normalize(&self.settings.live().room);
            let lights: Vec<HaEntity> = states
                .into_iter()
                .filter(|entity| entity.domain == "light")
                .filter(|entity| {
                    let id = normalize(&entity.entity_id.replace(['.', '_'], " "));
                    let name = normalize(&entity.friendly_name);
                    room.split_whitespace().any(|word| id.contains(word) || name.contains(word))
                })
                .take(10)
                .collect();
            return Ok(lights);
        }
        let room = self.settings.live().room;
        let matches = ha::rank_entities(&states, &query, &room, 5)
            .into_iter()
            .filter(|entity| TURN_DOMAINS.contains(&entity.domain.as_str()))
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            Ok(matches)
        } else {
            Ok(Vec::new())
        }
    }

    async fn media(&self, action_name: &str, action: MediaAction) -> Value {
        let Some(entity) = self.default_media().await else {
            return fail("no media player is configured");
        };
        self.media_entity(action_name, action, &entity).await
    }

    async fn media_entity(&self, action_name: &str, action: MediaAction, entity_id: &str) -> Value {
        let (service, mut data) = match action {
            MediaAction::Play => ("media_play", json!({})),
            MediaAction::Pause => ("media_pause", json!({})),
            MediaAction::Next => ("media_next_track", json!({})),
            MediaAction::Previous => ("media_previous_track", json!({})),
            MediaAction::VolumeUp => ("volume_up", json!({})),
            MediaAction::VolumeDown => ("volume_down", json!({})),
            MediaAction::SetVolume(level) => ("volume_set", json!({ "volume_level": level })),
        };
        if action_name == "play_pause" {
            return self
                .call_service("media_player", "media_play_pause", entity_id, json!({}), false)
                .await;
        }
        if let Some(object) = data.as_object_mut() {
            object.remove("entity_id");
        }
        self.call_service("media_player", service, entity_id, data, false)
            .await
    }

    async fn default_media(&self) -> Option<String> {
        if let Some(entity) = &self.settings.media_entity {
            return Some(entity.clone());
        }
        let ha = self.ha.as_ref()?;
        let states = ha.states(false).await.ok()?;
        states
            .iter()
            .find(|entity| entity.domain == "media_player" && entity.state == "playing")
            .or_else(|| first_domain(&states, "media_player"))
            .map(|entity| entity.entity_id.clone())
    }

    async fn todo(&self, args: &Value) -> Value {
        let Some(ha) = &self.ha else {
            return fail("home assistant is not configured");
        };
        let entity_id = match args.get("entity_id").and_then(|v| v.as_str()) {
            Some(entity) if !entity.is_empty() => entity.to_string(),
            _ => match &self.settings.todo_entity {
                Some(entity) => entity.clone(),
                None => return fail("todo entity_id is required"),
            },
        };
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("list");
        match action {
            "add" => {
                let item = args.get("item").and_then(|v| v.as_str()).unwrap_or("");
                if item.is_empty() {
                    return fail("item is required");
                }
                self.call_service(
                    "todo",
                    "add_item",
                    &entity_id,
                    json!({ "item": item }),
                    false,
                )
                .await
            }
            "complete" => {
                let item = args.get("item").and_then(|v| v.as_str()).unwrap_or("");
                if item.is_empty() {
                    return fail("item uid is required");
                }
                self.call_service(
                    "todo",
                    "update_item",
                    &entity_id,
                    json!({ "item": item, "status": "completed" }),
                    false,
                )
                .await
            }
            _ => match ha
                .call_service_result(
                    "todo",
                    "get_items",
                    json!({}),
                    json!({ "entity_id": entity_id }),
                )
                .await
            {
                Ok(result) => ok(result),
                Err(err) => fail(err.to_string()),
            },
        }
    }

    async fn morning_brief(&self) -> Value {
        let Some(ha) = &self.ha else {
            return ok(json!({
                "events": self.app.recent_events(),
                "timers": self.timer_values(),
                "note": "home assistant is not configured",
            }));
        };
        let states = match ha.states(true).await {
            Ok(states) => states,
            Err(err) => return fail(err.to_string()),
        };
        if let Some(people) = ha::presence_from(&states) {
            self.app.set_people(Some(people));
        }
        let weather = self
            .settings
            .weather_entity
            .as_ref()
            .and_then(|id| states.iter().find(|entity| &entity.entity_id == id))
            .or_else(|| first_domain(&states, "weather"))
            .map(trim_entity);
        let calendar_id = self
            .settings
            .calendar_entity
            .clone()
            .or_else(|| first_domain(&states, "calendar").map(|entity| entity.entity_id.clone()));
        let mut calendar = None;
        if let Some(entity_id) = calendar_id {
            let start = Utc::now();
            let end = start + ChronoDuration::hours(18);
            calendar = ha
                .calendar(&entity_id, &start.to_rfc3339(), &end.to_rfc3339())
                .await
                .ok();
        }
        ok(json!({
            "people_home": self.app.snapshot(Vec::new()).get("people_home").cloned(),
            "weather": weather,
            "calendar": calendar,
            "events": self.app.recent_events(),
            "timers": self.timer_values(),
        }))
    }

    async fn who_is_home(&self) -> Value {
        if let Some(ha) = &self.ha {
            if let Ok(states) = ha.states(true).await {
                self.app.set_people(ha::presence_from(&states));
            }
        }
        let snapshot = self.app.snapshot(Vec::new());
        ok(json!({ "people_home": snapshot.get("people_home").cloned() }))
    }

    async fn describe_camera(&self, requested: Option<&str>) -> Value {
        let Some(ha) = &self.ha else {
            return fail("home assistant is not configured");
        };
        let entity_id = match requested.filter(|entity| !entity.is_empty()) {
            Some(entity) => entity.to_string(),
            None => match self.pick_camera().await {
                Some(entity) => entity,
                None => return fail("no doorbell camera is configured"),
            },
        };
        let bytes = match ha.snapshot(&entity_id).await {
            Ok(bytes) => bytes,
            Err(err) => return fail(err.to_string()),
        };
        match vision::describe(
            &self.settings.xai_api_key,
            &self.settings.vision_model,
            &bytes,
            "You are looking through a home camera. In one short sentence, say who or what is visible. If it is unclear, say so.",
        )
        .await
        {
            Ok(text) => ok(json!({ "entity_id": entity_id, "description": text })),
            Err(err) => fail(vision::short_error(&err)),
        }
    }

    async fn pick_camera(&self) -> Option<String> {
        if let Some(entity) = &self.settings.doorbell_camera {
            return Some(entity.clone());
        }
        let states = self.ha.as_ref()?.states(false).await.ok()?;
        states
            .into_iter()
            .find(|entity| {
                entity.domain == "camera"
                    && (entity.entity_id.contains("doorbell") || entity.entity_id.contains("front"))
            })
            .map(|entity| entity.entity_id)
    }

    fn timer_values(&self) -> Vec<Value> {
        self.timers.list().iter().map(timer_value).collect()
    }

    fn remember(&self, key: String) {
        if let Ok(mut recent) = self.recent.lock() {
            recent.retain(|(_, at)| at.elapsed() < Duration::from_secs(8));
            recent.push((key, Instant::now()));
        }
    }

    fn recently_done(&self, key: &str) -> bool {
        self.recent
            .lock()
            .ok()
            .map(|recent| recent.iter().any(|(saved, at)| saved == key && at.elapsed() < Duration::from_secs(8)))
            .unwrap_or(false)
    }

    fn alias_lookup(&self, key: &str) -> Option<String> {
        self.aliases.lock().ok().and_then(|map| map.get(key).cloned())
    }

    pub fn alias_map(&self) -> HashMap<String, String> {
        self.aliases.lock().map(|map| map.clone()).unwrap_or_default()
    }

    pub fn replace_aliases(&self, raw: HashMap<String, String>) -> Result<HashMap<String, String>, String> {
        if raw.len() > 80 {
            return Err("eighty aliases is the limit".into());
        }
        let mut cleaned = HashMap::new();
        for (key, value) in raw {
            let key = normalize(&key);
            if key.is_empty() || key.chars().count() > 60 {
                return Err("an alias name is empty or too long".into());
            }
            let value = value.trim().to_string();
            check_alias_target(&value)?;
            cleaned.insert(key, value);
        }
        write_alias_file(&self.settings.aliases_file, &cleaned)?;
        if let Ok(mut map) = self.aliases.lock() {
            *map = cleaned.clone();
        }
        Ok(cleaned)
    }

    pub async fn answer_confirmation(&self, yes: bool) -> Value {
        let result = self.confirm(yes).await;
        self.audit_dashboard("confirm_action", json!({ "confirmed": yes }), &result);
        result
    }

    pub fn dashboard_add_timer(&self, seconds: u64, label: &str) -> Value {
        let label: String = label.trim().chars().take(60).collect();
        let label = if label.is_empty() { "timer".to_string() } else { label };
        let result = self.add_timer(seconds, &label);
        self.audit_dashboard("dashboard_timer", json!({ "seconds": seconds, "label": label }), &result);
        result
    }

    pub fn dashboard_cancel_timer(&self, id: u64) -> Value {
        let result = self.cancel_timer(&json!({ "id": id }));
        self.audit_dashboard("dashboard_timer", json!({ "cancel": id }), &result);
        result
    }

    pub async fn dashboard_lights(&self) -> Value {
        let Some(ha) = &self.ha else {
            return json!({
                "ok": false,
                "error": "home assistant is not configured",
                "lights": [],
                "room_match": false,
            });
        };
        let states = match ha.states(false).await {
            Ok(states) => states,
            Err(err) => return fail(err.to_string()),
        };
        let words: Vec<String> = normalize(&self.settings.live().room)
            .split_whitespace()
            .filter(|word| word.len() > 1)
            .map(str::to_string)
            .collect();
        let lights: Vec<&HaEntity> = states.iter().filter(|entity| entity.domain == "light").collect();
        let matched: Vec<&HaEntity> = lights
            .iter()
            .copied()
            .filter(|entity| light_matches(entity, &words))
            .take(12)
            .collect();
        let room_match = !matched.is_empty();
        let shown: Vec<&HaEntity> = if room_match {
            matched
        } else {
            lights.into_iter().take(12).collect()
        };
        json!({
            "ok": true,
            "room_match": room_match,
            "note": if room_match { "" } else { "No lights matched this room, so these are other lights." },
            "lights": shown.iter().map(|entity| light_value(entity)).collect::<Vec<_>>(),
        })
    }

    pub async fn dashboard_set_light(&self, entity_id: &str, on: bool) -> Value {
        if !entity_id.starts_with("light.") || entity_id.matches('.').count() != 1 {
            return fail("pick a light");
        }
        let service = if on { "turn_on" } else { "turn_off" };
        let result = self.call_service("light", service, entity_id, json!({}), false).await;
        self.audit_dashboard(
            "dashboard_light",
            json!({ "entity_id": entity_id, "service": service }),
            &result,
        );
        result
    }

    pub async fn dashboard_players(&self) -> Value {
        let Some(ha) = &self.ha else {
            return json!({
                "ok": false,
                "error": "Connect Home Assistant to control Apple Music, HomePod, or Music Assistant.",
                "players": [],
            });
        };
        let states = match ha.states(false).await {
            Ok(states) => states,
            Err(err) => return fail(err.to_string()),
        };
        let players: Vec<Value> = states
            .iter()
            .filter(|entity| entity.domain == "media_player")
            .take(20)
            .map(player_value)
            .collect();
        json!({ "ok": true, "players": players })
    }

    pub async fn dashboard_media(
        &self,
        entity_id: &str,
        action: &str,
        volume: Option<f64>,
        source: Option<&str>,
        query: Option<&str>,
    ) -> Value {
        if !entity_id.starts_with("media_player.") || entity_id.matches('.').count() != 1 {
            return fail("pick a media player");
        }
        let result = match action {
            "play" => self.call_service("media_player", "media_play", entity_id, json!({}), false).await,
            "pause" => self.call_service("media_player", "media_pause", entity_id, json!({}), false).await,
            "next" => {
                self.call_service("media_player", "media_next_track", entity_id, json!({}), false)
                    .await
            }
            "previous" => {
                self.call_service("media_player", "media_previous_track", entity_id, json!({}), false)
                    .await
            }
            "volume" => {
                let level = volume.unwrap_or(0.3).clamp(0.0, 1.0);
                self.call_service(
                    "media_player",
                    "volume_set",
                    entity_id,
                    json!({ "volume_level": level }),
                    false,
                )
                .await
            }
            "source" => {
                let source = source.unwrap_or("").trim();
                if source.is_empty() || source.chars().count() > 80 {
                    return fail("pick a source");
                }
                if !self.source_allowed(entity_id, source).await {
                    return fail("that source is not on this player");
                }
                self.call_service(
                    "media_player",
                    "select_source",
                    entity_id,
                    json!({ "source": source }),
                    false,
                )
                .await
            }
            "play_query" => {
                let query = query.unwrap_or("").trim();
                if query.is_empty() {
                    return fail("say what to play");
                }
                if query.chars().count() > 120 {
                    return fail("that request is too long");
                }
                self.call_service(
                    "media_player",
                    "play_media",
                    entity_id,
                    json!({ "media_content_id": query, "media_content_type": "music" }),
                    false,
                )
                .await
            }
            _ => return fail("unknown music action"),
        };
        self.audit_dashboard(
            "dashboard_music",
            json!({ "entity_id": entity_id, "action": action }),
            &result,
        );
        result
    }

    pub async fn dashboard_security(&self) -> Value {
        let Some(ha) = &self.ha else {
            return json!({
                "ok": false,
                "error": "Connect Home Assistant to see alarm panels.",
                "panels": [],
            });
        };
        let states = match ha.states(false).await {
            Ok(states) => states,
            Err(err) => return fail(err.to_string()),
        };
        let panels: Vec<Value> = states
            .iter()
            .filter(|entity| entity.domain == "alarm_control_panel")
            .take(12)
            .map(|entity| {
                json!({
                    "entity_id": entity.entity_id,
                    "name": display_name(entity),
                    "state": entity.state,
                })
            })
            .collect();
        json!({ "ok": true, "panels": panels })
    }

    pub async fn dashboard_arm(&self, entity_id: &str, service: &str, code: Option<&str>) -> Value {
        const SERVICES: &[&str] = &[
            "alarm_disarm",
            "alarm_arm_home",
            "alarm_arm_away",
            "alarm_arm_night",
        ];
        if !entity_id.starts_with("alarm_control_panel.") || entity_id.matches('.').count() != 1 {
            return fail("pick an alarm panel");
        }
        if !SERVICES.contains(&service) {
            return fail("that alarm action is not allowed");
        }
        let mut data = json!({});
        if let Some(code) = code.map(str::trim).filter(|code| !code.is_empty()) {
            if code.chars().count() > 16 || !code.chars().all(|ch| ch.is_ascii_alphanumeric()) {
                return fail("the alarm code looks wrong");
            }
            data["code"] = json!(code);
        }
        let result = self
            .call_service("alarm_control_panel", service, entity_id, data, false)
            .await;
        self.audit_dashboard(
            "dashboard_security",
            json!({ "entity_id": entity_id, "service": service }),
            &result,
        );
        result
    }

    async fn source_allowed(&self, entity_id: &str, source: &str) -> bool {
        let Some(ha) = &self.ha else {
            return true;
        };
        let Ok(states) = ha.states(false).await else {
            return true;
        };
        let Some(entity) = states.iter().find(|entity| entity.entity_id == entity_id) else {
            return false;
        };
        let list = source_names(entity);
        list.is_empty() || list.iter().any(|item| item == source)
    }

    fn audit_dashboard(&self, tool: &str, args: Value, result: &Value) {
        self.audit.record(&json!({
            "ts": Utc::now().to_rfc3339(),
            "room": self.settings.live().room,
            "tool": tool,
            "args": args,
            "ok": result.get("ok").and_then(|value| value.as_bool()).unwrap_or(false),
            "error": result.get("error"),
        }));
        let confirmed = result.get("needs_confirmation").and_then(|value| value.as_bool()) == Some(true);
        if result.get("ok").and_then(|value| value.as_bool()) == Some(true) || confirmed {
            self.app.set_last_tool(tool);
        }
    }
}

pub fn needs_confirmation(
    domain: &str,
    service: &str,
    entity_id: &str,
    domains: &[String],
    needles: &[String],
) -> bool {
    if domains.iter().any(|item| item == domain) {
        return true;
    }
    let haystack = format!("{service} {entity_id}").to_lowercase();
    needles
        .iter()
        .any(|needle| !needle.is_empty() && haystack.contains(&needle.to_lowercase()))
}

pub fn tool_definitions(settings: &Settings) -> Vec<Value> {
    let mut tools = vec![
        func(
            "search_entities",
            "Find Home Assistant entities by nickname or room. Call this before controlling a device when the entity id is not certain.",
            json!({ "query": { "type": "string" } }),
            &["query"],
        ),
        func(
            "get_state",
            "Read one Home Assistant entity.",
            json!({ "entity_id": { "type": "string" } }),
            &["entity_id"],
        ),
        func(
            "call_service",
            "Call an allowlisted Home Assistant service. Locks, alarms, and garage doors return needs_confirmation instead of running.",
            json!({
                "domain": { "type": "string" },
                "service": { "type": "string" },
                "entity_id": { "type": "string" },
                "data": { "type": "object" }
            }),
            &["domain", "service", "entity_id"],
        ),
        func(
            "confirm_action",
            "Run the pending lock, alarm, or garage action after the household clearly agrees.",
            json!({ "confirmed": { "type": "boolean" } }),
            &["confirmed"],
        ),
        func(
            "set_timer",
            "Start a local timer that chimes even if the voice session is closed.",
            json!({
                "seconds": { "type": "integer" },
                "minutes": { "type": "integer" },
                "label": { "type": "string" }
            }),
            &[],
        ),
        func(
            "cancel_timer",
            "Cancel a timer by id or label.",
            json!({ "id": { "type": "integer" }, "label": { "type": "string" } }),
            &[],
        ),
        func("list_timers", "List timers that have not fired.", json!({}), &[]),
        func(
            "media_control",
            "Control a media player: play, pause, play_pause, next, previous, volume_up, volume_down, set_volume.",
            json!({
                "action": { "type": "string" },
                "entity_id": { "type": "string" },
                "volume": { "type": "number", "description": "0 to 1 for set_volume" }
            }),
            &["action"],
        ),
        func(
            "todo_list",
            "Read or change a Home Assistant to-do list. Actions: list, add, complete.",
            json!({
                "action": { "type": "string" },
                "entity_id": { "type": "string" },
                "item": { "type": "string" }
            }),
            &["action"],
        ),
        func(
            "morning_brief",
            "Weather, upcoming calendar items, who is home, recent alerts, and active timers.",
            json!({}),
            &[],
        ),
        func("who_is_home", "Which people are home.", json!({}), &[]),
        func(
            "describe_camera",
            "Look at a camera snapshot and describe it in one sentence. Defaults to the doorbell camera.",
            json!({ "entity_id": { "type": "string" } }),
            &[],
        ),
        func(
            "recent_events",
            "Alerts, doorbell descriptions, and timers recorded since the assistant started.",
            json!({}),
            &[],
        ),
    ];
    let prefs = settings.live();
    if prefs.enable_web_search {
        tools.insert(0, json!({ "type": "web_search" }));
    }
    if prefs.enable_x_search {
        tools.insert(0, json!({ "type": "x_search" }));
    }
    if let Some(collection) = &settings.collection_id {
        tools.push(json!({
            "type": "file_search",
            "vector_store_ids": [collection],
            "max_num_results": 5
        }));
    }
    if let Some(url) = &settings.ha_mcp_url {
        if !settings.ha_mcp_tools.is_empty() {
            let mut mcp = json!({
                "type": "mcp",
                "server_url": url,
                "server_label": "home-assistant",
                "server_description": "Read-only Home Assistant tools. Do not use these to unlock, disarm, or open doors.",
                "allowed_tools": settings.ha_mcp_tools,
            });
            if let Some(token) = &settings.ha_mcp_token {
                mcp["authorization"] = json!(format!("Bearer {token}"));
            }
            tools.push(mcp);
        }
    }
    tools
}

pub fn instructions(settings: &Settings, people: Option<&[String]>, quiet: bool) -> String {
    let prefs = settings.live();
    let presence = match people {
        Some(people) if people.is_empty() => "nobody".to_string(),
        Some(people) => people.join(", "),
        None => "unknown".to_string(),
    };
    let phrase = if prefs.wake_phrase.is_empty() {
        "The household did not set a wake phrase.".to_string()
    } else {
        format!(
            "Only control devices after the person says \"{}\" in this turn or the previous few seconds.",
            prefs.wake_phrase
        )
    };
    format!(
        "You are the household voice assistant in the {room}. \
Be brief. Confirm each action in one short sentence. \
People home: {presence}. Quiet hours: {quiet}. \
{phrase} \
Use search_entities before guessing an entity id. \
Nicknames in the alias file are exact. \
Never unlock, disarm, or open a garage or door until confirm_action says the household agreed. \
Use the timer, media, to-do, camera, and morning_brief tools for those jobs. \
During quiet hours keep speech especially short.",
        room = prefs.room,
        quiet = if quiet { "yes" } else { "no" },
    )
}

pub fn keyterms(settings: &Settings, aliases: &HashMap<String, String>) -> Vec<String> {
    let prefs = settings.live();
    let mut terms = Vec::new();
    if !prefs.wake_phrase.is_empty() {
        terms.push(prefs.wake_phrase.clone());
    }
    terms.push(prefs.room.clone());
    for name in aliases.keys() {
        if terms.len() >= 100 {
            break;
        }
        let term: String = name.chars().take(50).collect();
        if !term.is_empty() {
            terms.push(term);
        }
    }
    terms
}

fn func(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "name": name,
        "description": description,
        "parameters": {
            "type": "object",
            "properties": properties,
            "required": required,
        }
    })
}

fn ok(value: Value) -> Value {
    let mut object = match value {
        Value::Object(map) => map,
        other => return json!({ "ok": true, "result": other }),
    };
    object.insert("ok".into(), Value::Bool(true));
    Value::Object(object)
}

fn fail(message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": message.into() })
}

fn check_alias_target(value: &str) -> Result<(), String> {
    const DENIED: &[&str] = &[
        "shell_command",
        "hassio",
        "command_line",
        "python_script",
        "rest_command",
    ];
    let mut parts = value.split('.');
    let domain = parts.next().unwrap_or("");
    let object = parts.next().unwrap_or("");
    if parts.next().is_some() || domain.is_empty() || object.is_empty() {
        return Err("an alias must be one domain.object id".into());
    }
    let domain_ok = domain
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_');
    let object_ok = object
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_');
    if !domain_ok || !object_ok {
        return Err("alias targets must be ascii entity ids".into());
    }
    if DENIED.contains(&domain) {
        return Err(format!("{domain} is not allowed"));
    }
    if !ALLOWED_DOMAINS.contains(&domain) {
        return Err(format!("{domain} is not on the voice allowlist"));
    }
    Ok(())
}

fn write_alias_file(path: &Path, map: &HashMap<String, String>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
    }
    let body = serde_json::to_vec_pretty(map).map_err(|err| err.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|err| err.to_string())?;
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path).map_err(|err| err.to_string())
}

fn light_matches(entity: &HaEntity, words: &[String]) -> bool {
    if words.is_empty() {
        return false;
    }
    let id = normalize(&entity.entity_id.replace(['.', '_'], " "));
    let name = normalize(&entity.friendly_name);
    words.iter().any(|word| id.contains(word) || name.contains(word))
}

fn light_value(entity: &HaEntity) -> Value {
    json!({
        "entity_id": entity.entity_id,
        "name": display_name(entity),
        "state": entity.state,
        "on": entity.state == "on",
    })
}

fn display_name(entity: &HaEntity) -> String {
    if entity.friendly_name.is_empty() {
        entity.entity_id.clone()
    } else {
        entity.friendly_name.clone()
    }
}

fn attr_str(entity: &HaEntity, key: &str) -> String {
    entity
        .attributes
        .get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

fn source_names(entity: &HaEntity) -> Vec<String> {
    entity
        .attributes
        .get("source_list")
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|value| value.as_str())
                .map(|text| text.chars().take(80).collect())
                .take(40)
                .collect()
        })
        .unwrap_or_default()
}

fn music_badge(entity: &HaEntity) -> Option<&'static str> {
    let blob = format!(
        "{} {} {} {} {}",
        attr_str(entity, "app_name").to_lowercase(),
        attr_str(entity, "source").to_lowercase(),
        source_names(entity).join(" ").to_lowercase(),
        entity.entity_id.to_lowercase(),
        entity.friendly_name.to_lowercase()
    );
    if blob.contains("music assistant") || blob.contains("music_assistant") {
        Some("Music Assistant")
    } else if blob.contains("apple") || blob.contains("homepod") {
        Some("Apple Music")
    } else {
        None
    }
}

fn player_value(entity: &HaEntity) -> Value {
    json!({
        "entity_id": entity.entity_id,
        "name": display_name(entity),
        "state": entity.state,
        "volume": entity.attributes.get("volume_level").and_then(|value| value.as_f64()),
        "title": attr_str(entity, "media_title"),
        "artist": attr_str(entity, "media_artist"),
        "app": attr_str(entity, "app_name"),
        "source": attr_str(entity, "source"),
        "sources": source_names(entity),
        "badge": music_badge(entity),
    })
}

fn timer_value(timer: &Timer) -> Value {
    json!({
        "id": timer.id,
        "label": timer.label,
        "fire_at_unix": timer.fire_at_unix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_and_garage_doors_need_confirmation() {
        let domains = vec!["lock".into(), "alarm_control_panel".into()];
        let needles = vec!["garage_door".into(), "front_door".into()];
        assert!(needs_confirmation(
            "lock",
            "unlock",
            "lock.front_door",
            &domains,
            &needles
        ));
        assert!(needs_confirmation(
            "cover",
            "open_cover",
            "cover.garage_door",
            &domains,
            &needles
        ));
        assert!(!needs_confirmation(
            "light",
            "turn_off",
            "light.kitchen",
            &domains,
            &needles
        ));
    }

    #[test]
    fn tool_list_includes_the_household_functions() {
        let mut settings = sample_settings();
        settings.enable_web_search = true;
        settings.collection_id = Some("col_1".into());
        settings.ha_mcp_url = Some("https://ha.local/mcp".into());
        settings.ha_mcp_tools = vec!["get_state".into()];
        let tools = tool_definitions(&settings);
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(|value| value.as_str()))
            .collect();
        for required in ["search_entities", "confirm_action", "set_timer", "describe_camera"] {
            assert!(names.contains(&required), "missing {required}");
        }
        assert!(tools.iter().any(|tool| tool.get("type").and_then(|v| v.as_str()) == Some("file_search")));
        assert!(tools.iter().any(|tool| tool.get("type").and_then(|v| v.as_str()) == Some("mcp")));
    }

    #[test]
    fn shell_commands_cannot_be_aliases() {
        let path = std::env::temp_dir().join(format!("gha-aliases-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut settings = Settings::fixture();
        settings.aliases_file = path.clone();
        let timers = std::env::temp_dir().join(format!("gha-timers-alias-{}.json", std::process::id()));
        let audit = std::env::temp_dir().join(format!("gha-audit-alias-{}.jsonl", std::process::id()));
        let host = ToolHost::new(
            settings,
            AppState::new("office"),
            None,
            TimerStore::load(&timers).unwrap(),
            AuditLog::open(&audit),
            HashMap::new(),
        );
        let err = host
            .replace_aliases(HashMap::from([("kitchen".into(), "shell_command.boom".into())]))
            .unwrap_err();
        assert!(err.contains("not allowed"));
        let saved = host
            .replace_aliases(HashMap::from([("Kitchen Lamp".into(), "light.kitchen".into())]))
            .unwrap();
        assert_eq!(saved.get("kitchen lamp").map(String::as_str), Some("light.kitchen"));
        assert_eq!(host.alias_lookup("kitchen lamp").as_deref(), Some("light.kitchen"));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(timers);
        let _ = std::fs::remove_file(audit);
    }

    #[tokio::test]
    async fn dashboard_alarm_actions_are_limited() {
        let timers = std::env::temp_dir().join(format!("gha-timers-arm-{}.json", std::process::id()));
        let audit = std::env::temp_dir().join(format!("gha-audit-arm-{}.jsonl", std::process::id()));
        let host = ToolHost::new(
            Settings::fixture(),
            AppState::new("office"),
            None,
            TimerStore::load(&timers).unwrap(),
            AuditLog::open(&audit),
            HashMap::new(),
        );
        let blocked = host
            .dashboard_arm("alarm_control_panel.home", "alarm_trigger", None)
            .await;
        assert_eq!(blocked["ok"], false);
        assert!(blocked["error"].as_str().unwrap_or("").contains("not allowed"));
        let wrong = host
            .dashboard_arm("light.kitchen", "alarm_arm_home", Some("1234"))
            .await;
        assert!(wrong["error"].as_str().unwrap_or("").contains("alarm panel"));
        let _ = std::fs::remove_file(timers);
        let _ = std::fs::remove_file(audit);
    }

    #[test]
    fn apple_and_music_assistant_get_a_badge() {
        let apple = HaEntity {
            entity_id: "media_player.living_room".into(),
            state: "playing".into(),
            friendly_name: "Living room".into(),
            domain: "media_player".into(),
            attributes: json!({ "app_name": "Apple Music", "source_list": ["Apple Music"] }),
        };
        let assistant = HaEntity {
            entity_id: "media_player.kitchen".into(),
            state: "idle".into(),
            friendly_name: "Kitchen".into(),
            domain: "media_player".into(),
            attributes: json!({ "source": "Music Assistant" }),
        };
        assert_eq!(music_badge(&apple), Some("Apple Music"));
        assert_eq!(music_badge(&assistant), Some("Music Assistant"));
    }

    fn sample_settings() -> Settings {
        Settings::fixture()
    }
}
