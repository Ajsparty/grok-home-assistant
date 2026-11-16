use crate::alarms::AlarmStore;
use crate::console::{self, ConsoleState};
use crate::lock::ConsoleGuard;
use crate::tools::ToolHost;
use tokio::sync::watch;

pub async fn serve(
    bind: String,
    host: ToolHost,
    alarms: AlarmStore,
    guard: ConsoleGuard,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    if bind.is_empty() {
        let _ = shutdown.changed().await;
        return Ok(());
    }
    let router = console::router(ConsoleState { host, alarms, guard });
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    log::info!("status page at http://{bind}");
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = shutdown.changed().await;
        })
        .await?;
    Ok(())
}
