use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

const ATTR_KEYS: &[&str] = &[
    "friendly_name",
    "temperature",
    "current_temperature",
    "humidity",
    "hvac_mode",
    "hvac_action",
    "brightness",
    "color_temp_kelvin",
    "volume_level",
    "media_title",
    "media_artist",
    "device_class",
    "unit_of_measurement",
    "percentage",
    "fan_mode",
];

#[derive(Clone, Debug)]
pub struct HaEntity {
    pub entity_id: String,
    pub state: String,
    pub friendly_name: String,
    pub domain: String,
    pub attributes: Value,
}

#[derive(Clone)]
pub struct HaClient {
    http: Client,
    base: String,
    token: String,
    cache: Arc<Mutex<Option<(Instant, Vec<HaEntity>)>>>,
}

impl HaClient {
    pub fn new(base: &str, token: &str) -> Result<Self> {
        let base = base.trim_end_matches('/').to_string();
        if base.is_empty() {
            return Err(anyhow!("HA_URL is empty"));
        }
        Ok(Self {
            http: Client::builder().timeout(Duration::from_secs(20)).build()?,
            base,
            token: token.to_string(),
            cache: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn states(&self, force: bool) -> Result<Vec<HaEntity>> {
        if !force {
            if let Some(cached) = self.cached() {
                return Ok(cached);
            }
        }
        let url = format!("{}/api/states", self.base);
        let response = self.http.get(url).bearer_auth(&self.token).send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("home assistant states {status}: {body}"));
        }
        let payload: Vec<Value> = response.json().await?;
        let entities: Vec<_> = payload.iter().filter_map(parse_entity).collect();
        if let Ok(mut slot) = self.cache.lock() {
            *slot = Some((Instant::now(), entities.clone()));
        }
        Ok(entities)
    }

    pub async fn search(&self, query: &str, room: &str, limit: usize) -> Result<Vec<HaEntity>> {
        let states = self.states(false).await?;
        Ok(rank_entities(&states, query, room, limit))
    }

    pub async fn call_service(&self, domain: &str, service: &str, body: Value) -> Result<Value> {
        let url = format!("{}/api/services/{domain}/{service}", self.base);
        let response = self
            .http
            .post(url)
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("home assistant {domain}.{service} {status}: {text}"));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    pub async fn call_service_result(
        &self,
        domain: &str,
        service: &str,
        service_data: Value,
        target: Value,
    ) -> Result<Value> {
        let call = self.call_service_ws(domain, service, service_data, target);
        match tokio::time::timeout(Duration::from_secs(12), call).await {
            Ok(result) => result,
            Err(_) => Err(anyhow!("home assistant websocket call timed out")),
        }
    }

    pub async fn snapshot(&self, entity_id: &str) -> Result<Vec<u8>> {
        let url = format!("{}/api/camera_proxy/{entity_id}", self.base);
        let response = self.http.get(url).bearer_auth(&self.token).send().await?;
        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!("camera {entity_id} returned {status}"));
        }
        let bytes = response.bytes().await?;
        if bytes.len() > 5_000_000 {
            return Err(anyhow!("camera snapshot is larger than 5 MB"));
        }
        Ok(bytes.to_vec())
    }

    pub async fn calendar(&self, entity_id: &str, start: &str, end: &str) -> Result<Value> {
        let url = format!("{}/api/calendars/{entity_id}", self.base);
        let response = self
            .http
            .get(url)
            .bearer_auth(&self.token)
            .query(&[("start", start), ("end", end)])
            .send()
            .await?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("calendar {status}: {text}"));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    fn cached(&self) -> Option<Vec<HaEntity>> {
        let slot = self.cache.lock().ok()?;
        let (at, entities) = slot.as_ref()?;
        if at.elapsed() < Duration::from_secs(8) {
            Some(entities.clone())
        } else {
            None
        }
    }

    async fn call_service_ws(
        &self,
        domain: &str,
        service: &str,
        service_data: Value,
        target: Value,
    ) -> Result<Value> {
        let ws_url = to_ws(&self.base)?;
        let (mut socket, _) = connect_async(ws_url.as_str()).await?;
        let mut sent_call = false;
        loop {
            let Some(message) = socket.next().await else {
                return Err(anyhow!("home assistant websocket closed"));
            };
            let Message::Text(text) = message? else {
                continue;
            };
            let event: Value = serde_json::from_str(&text)?;
            match event.get("type").and_then(|kind| kind.as_str()) {
                Some("auth_required") => {
                    socket
                        .send(Message::Text(
                            json!({"type": "auth", "access_token": self.token})
                                .to_string()
                                .into(),
                        ))
                        .await?;
                }
                Some("auth_invalid") => return Err(anyhow!("home assistant rejected the token")),
                Some("auth_ok") if !sent_call => {
                    socket
                        .send(Message::Text(
                            json!({
                                "id": 1,
                                "type": "call_service",
                                "domain": domain,
                                "service": service,
                                "service_data": service_data,
                                "target": target,
                                "return_response": true
                            })
                            .to_string()
                            .into(),
                        ))
                        .await?;
                    sent_call = true;
                }
                Some("result") => {
                    if event.get("success") == Some(&Value::Bool(true)) {
                        return Ok(event.get("result").cloned().unwrap_or(Value::Null));
                    }
                    return Err(anyhow!(
                        "home assistant call failed: {}",
                        event.get("error").unwrap_or(&Value::Null)
                    ));
                }
                _ => {}
            }
        }
    }
}

pub fn rank_entities(states: &[HaEntity], query: &str, room: &str, limit: usize) -> Vec<HaEntity> {
    let mut scored: Vec<(i32, &HaEntity)> = states
        .iter()
        .filter_map(|entity| {
            let score = score_entity(entity, query, room);
            (score > 0).then_some((score, entity))
        })
        .collect();
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.entity_id.cmp(&right.1.entity_id))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, entity)| entity.clone())
        .collect()
}

pub fn score_entity(entity: &HaEntity, query: &str, room: &str) -> i32 {
    let query = crate::textutil::normalize(query);
    if query.is_empty() {
        return 0;
    }
    let name = crate::textutil::normalize(&entity.friendly_name);
    let id = crate::textutil::normalize(&entity.entity_id.replace(['.', '_'], " "));
    let mut score = if name == query || id == query {
        100
    } else if name.contains(&query) || id.contains(&query) {
        50
    } else {
        let words: Vec<&str> = query.split_whitespace().collect();
        if words.iter().all(|word| name.contains(word) || id.contains(word)) {
            30
        } else {
            return 0;
        }
    };
    let room = crate::textutil::normalize(room);
    if !room.is_empty()
        && room
            .split_whitespace()
            .any(|word| id.contains(word) || name.contains(word))
    {
        score += 20;
    }
    score
}

pub fn trim_entity(entity: &HaEntity) -> Value {
    let mut attributes = serde_json::Map::new();
    if let Some(object) = entity.attributes.as_object() {
        for key in ATTR_KEYS {
            if let Some(value) = object.get(*key) {
                attributes.insert((*key).to_string(), value.clone());
            }
        }
    }
    json!({
        "entity_id": entity.entity_id,
        "state": entity.state,
        "friendly_name": entity.friendly_name,
        "attributes": attributes,
    })
}

pub fn presence_from(states: &[HaEntity]) -> Option<Vec<String>> {
    let people: Vec<&HaEntity> = states.iter().filter(|entity| entity.domain == "person").collect();
    if people.is_empty() {
        return None;
    }
    Some(
        people
            .into_iter()
            .filter(|entity| entity.state == "home")
            .map(|entity| {
                if entity.friendly_name.is_empty() {
                    entity.entity_id.clone()
                } else {
                    entity.friendly_name.clone()
                }
            })
            .collect(),
    )
}

pub fn first_domain<'a>(states: &'a [HaEntity], domain: &str) -> Option<&'a HaEntity> {
    states.iter().find(|entity| entity.domain == domain)
}

fn parse_entity(value: &Value) -> Option<HaEntity> {
    let entity_id = value.get("entity_id")?.as_str()?.to_string();
    let state = value.get("state").and_then(|item| item.as_str()).unwrap_or("").to_string();
    let attributes = value.get("attributes").cloned().unwrap_or_else(|| json!({}));
    let friendly_name = attributes
        .get("friendly_name")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .to_string();
    let domain = entity_id.split('.').next().unwrap_or("").to_string();
    Some(HaEntity {
        entity_id,
        state,
        friendly_name,
        domain,
        attributes,
    })
}

fn to_ws(base: &str) -> Result<Url> {
    let http = Url::parse(base)?;
    let scheme = match http.scheme() {
        "https" => "wss",
        _ => "ws",
    };
    let mut ws = http;
    ws.set_scheme(scheme)
        .map_err(|()| anyhow!("could not build the home assistant websocket url"))?;
    ws.set_path("/api/websocket");
    ws.set_query(None);
    Ok(ws)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(id: &str, name: &str) -> HaEntity {
        let domain = id.split('.').next().unwrap_or("").to_string();
        HaEntity {
            entity_id: id.into(),
            state: "off".into(),
            friendly_name: name.into(),
            domain,
            attributes: json!({}),
        }
    }

    #[test]
    fn room_lights_rank_above_other_rooms() {
        let states = vec![
            entity("light.kitchen_ceiling", "Kitchen Ceiling"),
            entity("light.office_lamp", "Office Lamp"),
        ];
        let found = rank_entities(&states, "lamp", "office", 4);
        assert_eq!(found[0].entity_id, "light.office_lamp");
    }

    #[test]
    fn unrelated_names_score_zero() {
        let lamp = entity("light.office_lamp", "Office Lamp");
        assert_eq!(score_entity(&lamp, "garage door", ""), 0);
    }

    #[test]
    fn presence_ignores_houses_without_people() {
        let states = vec![entity("light.kitchen", "Kitchen")];
        assert!(presence_from(&states).is_none());
    }
}
