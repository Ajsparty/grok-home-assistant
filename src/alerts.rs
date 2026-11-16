use crate::announce::Speaker;
use crate::config::Settings;
use crate::ha::{self, HaClient, HaEntity};
use crate::state::AppState;
use crate::vision;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::watch;

pub async fn run(
    settings: Settings,
    ha: Option<HaClient>,
    app: AppState,
    speaker: Speaker,
    mut shutdown: watch::Receiver<bool>,
) {
    let Some(ha) = ha else {
        log::info!("alert watcher idle because home assistant is not configured");
        let _ = shutdown.changed().await;
        return;
    };
    let mut previous: HashMap<String, String> = HashMap::new();
    let mut primed = false;
    let mut last_seen: HashMap<String, Instant> = HashMap::new();
    loop {
        if *shutdown.borrow() {
            break;
        }
        app.refresh_quiet(&settings);
        match ha.states(true).await {
            Ok(states) => {
                app.set_people(ha::presence_from(&states));
                if !primed {
                    for entity in &states {
                        previous.insert(entity.entity_id.clone(), entity.state.clone());
                    }
                    primed = true;
                } else {
                    for entity in &states {
                        let old = previous.get(&entity.entity_id).cloned();
                        let changed = old.as_deref() != Some(entity.state.as_str());
                        if changed && is_watched(entity, &settings) && is_active(&entity.state) {
                            let recent = last_seen
                                .get(&entity.entity_id)
                                .map(|at| at.elapsed() < Duration::from_secs(20))
                                .unwrap_or(false);
                            if !recent {
                                last_seen.insert(entity.entity_id.clone(), Instant::now());
                                announce(&settings, &ha, &speaker, entity).await;
                            }
                        }
                        previous.insert(entity.entity_id.clone(), entity.state.clone());
                    }
                }
            }
            Err(err) => log::warn!("alert poll failed: {err:#}"),
        }
        tokio::select! {
            _ = tokio::time::sleep(settings.alert_poll) => {}
            _ = shutdown.changed() => {}
        }
    }
}

async fn announce(settings: &Settings, ha: &HaClient, speaker: &Speaker, entity: &HaEntity) {
    let critical = is_critical(entity, settings);
    let name = if entity.friendly_name.is_empty() {
        entity.entity_id.as_str()
    } else {
        entity.friendly_name.as_str()
    };
    if is_doorbell(entity, settings) {
        let description = match camera_id(settings, ha).await {
            Some(camera) => match ha.snapshot(&camera).await {
                Ok(bytes) => {
                    let path = settings.data_dir.join("last-doorbell.jpg");
                    if let Some(parent) = path.parent() {
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }
                    let _ = tokio::fs::write(&path, &bytes).await;
                    let caption = match vision::describe(
                        &settings.xai_api_key,
                        &settings.vision_model,
                        &bytes,
                        "This is a door camera. In one short sentence, say who or what is there. If it is unclear, say so.",
                    )
                    .await
                    {
                        Ok(text) => text,
                        Err(err) => format!("I could not tell. {}", vision::short_error(&err)),
                    };
                    speaker.app.set_doorbell(&caption);
                    caption
                }
                Err(err) => format!("The camera did not answer. {}", vision::short_error(&err)),
            },
            None => "No camera is configured.".to_string(),
        };
        speaker.alert(format!("{name}. {description}"), critical);
        return;
    }
    speaker.alert(format!("{name} is {}.", entity.state), critical);
}

async fn camera_id(settings: &Settings, ha: &HaClient) -> Option<String> {
    if let Some(entity) = &settings.doorbell_camera {
        return Some(entity.clone());
    }
    let states = ha.states(false).await.ok()?;
    states
        .into_iter()
        .find(|entity| {
            entity.domain == "camera"
                && (entity.entity_id.contains("doorbell") || entity.entity_id.contains("front"))
        })
        .map(|entity| entity.entity_id)
}

fn is_active(state: &str) -> bool {
    matches!(
        state,
        "on" | "open" | "detected" | "unsafe" | "wet" | "smoke" | "gas" | "problem"
    )
}

fn haystack(entity: &HaEntity) -> String {
    format!("{} {}", entity.entity_id, entity.friendly_name).to_lowercase()
}

fn is_watched(entity: &HaEntity, settings: &Settings) -> bool {
    settings
        .alert_entities
        .iter()
        .any(|id| id == &entity.entity_id)
        || settings
            .alert_match
            .iter()
            .any(|needle| !needle.is_empty() && haystack(entity).contains(&needle.to_lowercase()))
}

fn is_critical(entity: &HaEntity, settings: &Settings) -> bool {
    let text = haystack(entity);
    settings
        .critical_match
        .iter()
        .any(|needle| !needle.is_empty() && text.contains(&needle.to_lowercase()))
}

fn is_doorbell(entity: &HaEntity, settings: &Settings) -> bool {
    settings
        .doorbell_entities
        .iter()
        .any(|id| id == &entity.entity_id)
        || haystack(entity).contains("doorbell")
}
