use crate::alarms::AlarmStore;
use crate::alerts;
use crate::announce::Speaker;
use crate::audio_hub;
use crate::audit::AuditLog;
use crate::config::Settings;
use crate::grok;
use crate::ha::HaClient;
use crate::health;
use crate::lock::ConsoleGuard;
use crate::satellite;
use crate::state::AppState;
use crate::timers::TimerStore;
use crate::tools::{self, ToolHost};
use anyhow::Result;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

pub async fn run() -> Result<()> {
    dotenvy::dotenv().ok();
    env_logger::init();
    let settings = Settings::from_env()?;
    let _ = tokio::fs::create_dir_all(&settings.log_dir).await;
    let _ = tokio::fs::create_dir_all(&settings.data_dir).await;

    log::info!(
        "starting in {} with voice {} and wake mode {:?}",
        settings.room,
        settings.voice,
        settings.wake_mode
    );
    if settings.ha_url.is_none() || settings.ha_token.is_none() {
        log::info!("home assistant credentials are not set; device tools will decline");
    }

    let app = AppState::new(settings.live().room);
    let audit = AuditLog::open(&settings.log_dir.join("audit.jsonl"));
    let aliases = tools::load_aliases(&settings.aliases_file);
    log::info!(
        "loaded {} device aliases from {}",
        aliases.len(),
        settings.aliases_file.display()
    );
    let ha = match (&settings.ha_url, &settings.ha_token) {
        (Some(url), Some(token)) => Some(HaClient::new(url, token)?),
        _ => None,
    };
    let timers = TimerStore::load(settings.data_dir.join("timers.json"))?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let links = audio_hub::spawn(settings.clone(), app.clone(), shutdown_rx.clone());

    #[cfg(feature = "local-audio")]
    crate::local_audio::start(links.handle.clone());

    let (announce_tx, announce_rx) = mpsc::channel(16);
    let speaker = Speaker {
        settings: settings.clone(),
        app: app.clone(),
        audio: links.handle.clone(),
        tx: announce_tx,
    };
    timers.clone().spawn(speaker.clone(), shutdown_rx.clone());
    let alarms = AlarmStore::load(settings.data_dir.join("alarms.json"))?;
    let health_alarms = alarms.clone();
    alarms.spawn(speaker.clone(), shutdown_rx.clone());
    let guard = ConsoleGuard::open(settings.data_dir.join("console-lock.json"));
    let (lock_on, lock_from_env) = guard.status();
    if lock_on && lock_from_env {
        log::info!("dashboard password comes from the environment");
    } else if !lock_on {
        log::warn!("dashboard has no password; set DASHBOARD_PASSWORD or one in Settings");
    }

    let host = ToolHost::new(settings.clone(), app.clone(), ha.clone(), timers.clone(), audit, aliases);
    let health_bind = settings.health_bind.clone();
    let health_host = host.clone();
    let health_shutdown = shutdown_rx.clone();
    let health_task = tokio::spawn(async move {
        if let Err(err) = health::serve(
            health_bind,
            health_host,
            health_alarms,
            guard,
            health_shutdown,
        )
        .await
        {
            log::error!("status page stopped: {err:#}");
        }
    });
    let satellite_bind = settings.satellite_bind.clone();
    let satellite_audio = links.handle.clone();
    let satellite_shutdown = shutdown_rx.clone();
    let satellite_task = tokio::spawn(async move {
        if let Err(err) = satellite::serve(satellite_bind, satellite_audio, satellite_shutdown).await {
            log::error!("satellite listener stopped: {err:#}");
        }
    });
    let alerts_task = tokio::spawn(alerts::run(settings, ha, app, speaker, shutdown_rx.clone()));

    tokio::select! {
        result = grok::run(host, links.mic_rx, links.barge_rx, announce_rx, links.handle, links.wake, shutdown_rx) => {
            if let Err(err) = result {
                log::error!("voice loop stopped: {err:#}");
            }
        }
        _ = wait_for_shutdown() => log::info!("shutting down"),
    }
    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        let _ = health_task.await;
        let _ = satellite_task.await;
        let _ = alerts_task.await;
    })
    .await;
    Ok(())
}

async fn wait_for_shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate()).expect("listen for sigterm");
        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}
