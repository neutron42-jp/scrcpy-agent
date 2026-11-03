use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;

use scrcpy_agent::app::AppState;
use scrcpy_agent::config::Config;

#[derive(Parser, Debug)]
#[command(
    name = "scrcpy-agent",
    version,
    about = "Android control MCP server with a native GTK4 window, backed by scrcpy"
)]
struct Cli {
    /// Run without the window; starts the MCP server immediately.
    #[arg(long)]
    headless: bool,
    /// Device serial to connect at startup (headless mode).
    #[arg(long)]
    serial: Option<String>,
    /// Port for the MCP server (default 18765).
    #[arg(long)]
    mcp_port: Option<u16>,
    /// Bind address for the MCP server (default 127.0.0.1).
    #[arg(long)]
    mcp_bind: Option<String>,
    /// adb executable to use.
    #[arg(long)]
    adb: Option<String>,
    /// Log level for stderr (error, warn, info, debug, trace).
    #[arg(long, default_value = "info")]
    log_level: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(&cli.log_level))
        .init();

    let mut config = Config::load();
    if let Some(port) = cli.mcp_port {
        config.mcp_port = port;
    }
    if let Some(bind) = &cli.mcp_bind {
        config.mcp_bind = bind.clone();
    }
    if let Some(adb) = &cli.adb {
        config.adb_path = adb.clone();
    }

    let state = AppState::new(config);

    if cli.headless {
        return run_headless(state, cli.serial);
    }

    #[cfg(feature = "gui")]
    {
        scrcpy_agent::ui::run(state.clone());
        state.close_session();
        state.stop_mcp();
        Ok(())
    }

    #[cfg(not(feature = "gui"))]
    {
        anyhow::bail!("this build has no GUI enabled; run with --headless");
    }
}

fn run_headless(state: Arc<AppState>, serial: Option<String>) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stop))?;
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&stop))?;

    let url = state.start_mcp()?;
    println!("MCP server listening on {url}");
    println!("register it with: transport streamable-http, url {url}");

    if let Some(serial) = serial {
        match state.open_session(&serial) {
            Ok(session) => {
                let status = session.status();
                println!(
                    "connected to {} ({}) state={}{}",
                    status.device_name,
                    status.serial,
                    status.state,
                    status
                        .dims
                        .map(|dims| format!(" {}x{}", dims.width, dims.height))
                        .unwrap_or_default()
                );
            }
            Err(err) => eprintln!("could not start session: {err:#}"),
        }
    }

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }

    state.close_session();
    state.stop_mcp();
    Ok(())
}
