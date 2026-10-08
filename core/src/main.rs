// Headless server for development and testing: `mixdeck-server [--data DIR] [--port N]`.
use mixdeck_core::{default_data_dir, start, HeadlessHost, Options};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned());
    let data = arg("--data").map(Into::into).unwrap_or_else(default_data_dir);
    let port = arg("--port").and_then(|p| p.parse().ok()).or_else(|| std::env::var("PORT").ok().and_then(|p| p.parse().ok())).unwrap_or(8787);
    let opts = Options::from_env(data, port);
    match start(opts, Arc::new(HeadlessHost)).await {
        Ok(app) => {
            println!("\n  MixDeck server running\n  Player: http://127.0.0.1:{0}/player\n  Remote: http://127.0.0.1:{0}/\n", app.port());
            tokio::signal::ctrl_c().await.ok();
            app.save_all();
        }
        Err(e) => { eprintln!("{e}"); std::process::exit(1); }
    }
}
