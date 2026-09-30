//! figma-canvas-mcp — read a live Figma canvas and reconstruct it as code.
//!
//! One Rust binary. It speaks MCP over stdio to the AI client, and holds a loopback
//! WebSocket that a small Figma plugin connects to. No Figma API token, no REST
//! rate limit, no Node.js runtime.

use figma_canvas_mcp::{DEFAULT_BRIDGE_PORT, bridge, doctor, tools};

use std::path::PathBuf;
use std::time::Duration;

use rmcp::{ServiceExt, transport::stdio};

const HELP: &str = "\
figma-canvas-mcp — read a live Figma canvas over a local plugin bridge

USAGE:
    figma-canvas-mcp [OPTIONS]
    figma-canvas-mcp doctor [OPTIONS]

COMMANDS:
    doctor                     Check the port, the plugin, the detected project,
                               the language server and the output directory, then
                               print the client config to copy. Run this first.

The server speaks MCP on stdin/stdout, so it is normally launched by an MCP
client rather than run by hand. Logs go to stderr.

OPTIONS:
    --port <PORT>              Bridge port for the Figma plugin [default: 18765]
    --host <HOST>              Bridge bind address [default: 127.0.0.1]
    --out-dir <DIR>            Where generate_code and export_assets write
                               [default: ./figma-out]
    --project-root <DIR>       The project whose stack and conventions generated
                               code should match [default: the working directory]
    --request-timeout <SECS>   How long to wait for the plugin [default: 30]
    -h, --help                 Print this help
    -V, --version              Print version

The bind address stays on loopback by design: the plugin runs on this machine.
";

struct Args {
    doctor: bool,
    host: String,
    port: u16,
    out_dir: PathBuf,
    project_root: PathBuf,
    timeout: Duration,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            doctor: false,
            host: "127.0.0.1".into(),
            port: DEFAULT_BRIDGE_PORT,
            out_dir: PathBuf::from("figma-out"),
            project_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            timeout: Duration::from_secs(30),
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut it = std::env::args().skip(1).peekable();
    if it.peek().map(|a| a == "doctor").unwrap_or(false) {
        it.next();
        args.doctor = true;
    }
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} expects a value"));
        match flag.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("figma-canvas-mcp {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--port" => {
                args.port = value()?
                    .parse()
                    .map_err(|e| format!("--port must be a number: {e}"))?
            }
            "--host" => args.host = value()?,
            "--out-dir" => args.out_dir = PathBuf::from(value()?),
            "--project-root" => args.project_root = PathBuf::from(value()?),
            "--request-timeout" => {
                let secs: u64 = value()?
                    .parse()
                    .map_err(|e| format!("--request-timeout must be seconds: {e}"))?;
                args.timeout = Duration::from_secs(secs);
            }
            other => return Err(format!("unknown argument: {other}\n\n{HELP}")),
        }
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    // stdout belongs to the MCP JSON-RPC stream. Every log line goes to stderr,
    // or the client sees corrupt protocol frames.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "figma_canvas_mcp=info".into()),
        )
        .with_ansi(false)
        .init();

    if args.doctor {
        let ok = doctor::run(&doctor::Options {
            host: args.host.clone(),
            port: args.port,
            out_dir: args.out_dir.clone(),
            project_root: args.project_root.clone(),
        })
        .await;
        std::process::exit(if ok { 0 } else { 1 });
    }

    let bridge = bridge::Bridge::new(args.timeout);

    // Bind before serving, so a port clash is a fact the tools can report rather
    // than a log line nobody sees. Several MCP clients being configured to launch
    // this binary is normal, and only one of them can hold the port.
    match bridge::Bridge::bind(&args.host, args.port).await {
        Ok((listener, addr)) => {
            tracing::info!("bridge listening on ws://{addr}");
            let bridge = bridge.clone();
            tokio::spawn(async move {
                if let Err(e) = bridge.serve_on(listener).await {
                    tracing::error!("bridge stopped: {e:#}");
                }
            });
        }
        Err(e) => {
            // Not fatal: the MCP server still answers, and figma_status explains.
            let reason = format!("{e:#}");
            tracing::error!("bridge could not start: {reason}");
            bridge.record_startup_error(reason).await;
        }
    }

    tracing::info!(
        "figma-canvas-mcp {} ready; bridge on ws://{}:{}, output to {}",
        env!("CARGO_PKG_VERSION"),
        args.host,
        args.port,
        args.out_dir.display()
    );

    let service = tools::FigmaServer::new(bridge, args.out_dir, args.project_root)
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("could not start MCP server: {e:?}"))?;

    service.waiting().await?;
    Ok(())
}
