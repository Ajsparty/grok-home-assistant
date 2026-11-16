#[tokio::main]
async fn main() {
    if let Err(err) = grok_home_assistant::run().await {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}
