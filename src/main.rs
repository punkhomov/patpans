use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use patpans::backend::sim::SimBackend;
use patpans::backend::{self, Backend};
use patpans::config::{self, FileConfig};
use patpans::daemon;
use patpans::ipc::{Client, Request, Response, StatusInfo};
use patpans::logging;
use patpans::paths;
use patpans::{BUILTIN_SCENARIOS, Config, Edge, Engine, Event};

#[derive(Parser)]
#[command(
    name = "patpans",
    version,
    about = "Software Snap Tap (SOCD cleaner): the last pressed key wins for opposing movement keys",
    after_help = "Built-in scenarios: run, sticky, taps, groups, repeat, passthrough, toggle\nExamples:\n  patpans run\n  patpans status\n  patpans simulate --builtin sticky\n  patpans simulate --script \"A+ D+ A- D-\""
)]
struct Cli {
    /// Path to the TOML config file (default: patpans.toml next to the executable)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Override the IPC endpoint (Unix socket path or Windows pipe name)
    #[arg(long, global = true, value_name = "ENDPOINT")]
    socket: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon (keyboard interception, IPC and tray)
    Run(RunArgs),
    /// Print the daemon status
    Status,
    /// Toggle interception on or off
    Toggle,
    /// Enable interception
    Enable,
    /// Disable interception
    Disable,
    /// Reload the config in the running daemon
    Reload,
    /// Stop the running daemon
    Stop,
    /// Replay an input script through the engine; no real keyboard needed
    Simulate(SimArgs),
    /// Validate the config and report usable keyboard devices
    Check,
}

#[derive(Args, Default)]
struct RunArgs {
    /// Override the toggle key from the config (`none` disables it)
    #[arg(long, value_name = "KEY")]
    toggle: Option<String>,
    /// Override groups, e.g. "A,D;W,S"
    #[arg(long, value_name = "PAIRS")]
    groups: Option<String>,
    /// Disable the sticky-keys restore behavior
    #[arg(long)]
    no_sticky: bool,
    /// Do not show the system tray icon
    #[arg(long)]
    no_tray: bool,
}

#[derive(Args)]
struct SimArgs {
    /// Script of key events such as "A+ D+ A- D-" (`+` press, `-` release)
    #[arg(long, value_name = "SCRIPT", conflicts_with = "builtin")]
    script: Option<String>,
    /// Built-in scenario name
    #[arg(long, value_name = "NAME")]
    builtin: Option<String>,
    /// Override the toggle key from the config (`none` disables it)
    #[arg(long, value_name = "KEY")]
    toggle: Option<String>,
    /// Override groups, e.g. "A,D;W,S"
    #[arg(long, value_name = "PAIRS")]
    groups: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or_else(paths::default_config_path);
    let endpoint = cli.socket.as_deref();
    let result = match &cli.command {
        Some(Command::Run(args)) => cmd_run(&config_path, args, endpoint),
        Some(Command::Status) => cmd_status(endpoint),
        Some(Command::Toggle) => cmd_request(&Request::Toggle, endpoint),
        Some(Command::Enable) => cmd_request(&Request::SetEnabled { enabled: true }, endpoint),
        Some(Command::Disable) => cmd_request(&Request::SetEnabled { enabled: false }, endpoint),
        Some(Command::Reload) => cmd_request(&Request::Reload, endpoint),
        Some(Command::Stop) => cmd_request(&Request::Stop, endpoint),
        Some(Command::Simulate(args)) => cmd_simulate(&config_path, args),
        Some(Command::Check) => cmd_check(&config_path),
        None => cmd_run(&config_path, &RunArgs::default(), endpoint),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn load_config(path: &Path) -> Result<Config> {
    FileConfig::load(path)?.into_config()
}

fn apply_overrides(
    config: &mut Config,
    groups: Option<&str>,
    toggle: Option<&str>,
    no_sticky: bool,
) -> Result<()> {
    if let Some(spec) = groups {
        config.groups = config::parse_groups(spec)?;
    }
    if let Some(name) = toggle {
        config.toggle = config::parse_toggle(name)?;
    }
    if no_sticky {
        config.sticky = false;
    }
    Ok(())
}

fn managed_keys(config: &Config) -> Vec<patpans::Key> {
    let mut keys: Vec<patpans::Key> = config.groups.iter().flat_map(|group| group.keys).collect();
    if let Some(toggle) = config.toggle {
        keys.push(toggle);
    }
    keys
}

fn cmd_run(path: &Path, args: &RunArgs, endpoint: Option<&str>) -> Result<()> {
    if !path.exists() {
        FileConfig::default()
            .save(path)
            .with_context(|| format!("failed to create `{}`", path.display()))?;
    }
    let mut config = load_config(path)?;
    apply_overrides(
        &mut config,
        args.groups.as_deref(),
        args.toggle.as_deref(),
        args.no_sticky,
    )?;
    let tray = config.tray && !args.no_tray;
    logging::init(Some(&paths::default_log_path(path)));
    let managed = managed_keys(&config);
    let options = daemon::Options {
        config_path: path.to_path_buf(),
        endpoint: endpoint.map(str::to_owned),
        tray,
    };
    let daemon = daemon::Daemon::spawn(config, options, move |control, commands| {
        backend::default_backend(&managed, tray, control, commands)
    })?;
    daemon.wait()
}

fn cmd_status(endpoint: Option<&str>) -> Result<()> {
    let mut client = Client::connect(endpoint)?;
    let status = status_of(client.request(&Request::Status)?)?;
    println!("patpans {} — daemon running", status.version);
    println!("  interception: {}", label(status.enabled));
    println!(
        "  elevated:     {}",
        if status.elevated { "yes" } else { "no" }
    );
    println!(
        "  toggle key:   {}",
        status.toggle.as_deref().unwrap_or("none")
    );
    println!(
        "  sticky:       {}",
        if status.sticky { "yes" } else { "no" }
    );
    println!("  tray:         {}", if status.tray { "yes" } else { "no" });
    println!("  config:       {}", status.config_path);
    for (index, group) in status.groups.iter().enumerate() {
        println!("  group #{}:     {}, {}", index + 1, group[0], group[1]);
    }
    Ok(())
}

fn cmd_request(request: &Request, endpoint: Option<&str>) -> Result<()> {
    let stop = matches!(request, Request::Stop);
    let mut client = Client::connect(endpoint)?;
    let response = client.request(request)?;
    if stop {
        if response.ok {
            println!("daemon stopped");
            return Ok(());
        }
        return Err(anyhow::anyhow!(response.error.unwrap_or_else(|| {
            "the daemon rejected the request".to_string()
        })));
    }
    let status = status_of(response)?;
    println!("snap tap: {}", label(status.enabled));
    Ok(())
}

fn status_of(response: Response) -> Result<StatusInfo> {
    if !response.ok {
        bail!(
            response
                .error
                .unwrap_or_else(|| "the daemon rejected the request".to_string())
        );
    }
    response.status.context("the daemon returned no status")
}

const fn label(enabled: bool) -> &'static str {
    if enabled { "ON" } else { "OFF" }
}

fn cmd_simulate(path: &Path, args: &SimArgs) -> Result<()> {
    let mut config = load_config(path)?;
    apply_overrides(
        &mut config,
        args.groups.as_deref(),
        args.toggle.as_deref(),
        false,
    )?;
    let script = resolve_script(args)?;
    let label = args.builtin.as_deref().unwrap_or(if args.script.is_some() {
        "custom"
    } else {
        "sticky (default)"
    });
    let mut sim = SimBackend::from_script(&script).context("invalid simulation script")?;
    let engine = Engine::new(config.groups, config.toggle, config.sticky);
    sim.run(engine)?;
    println!("scenario: {label}");
    println!("script:   {script}");
    print_trace(&sim);
    Ok(())
}

fn resolve_script(args: &SimArgs) -> Result<String> {
    if let Some(script) = &args.script {
        return Ok(script.clone());
    }
    let name = args.builtin.as_deref().unwrap_or("sticky");
    BUILTIN_SCENARIOS
        .iter()
        .find(|(scenario, _)| scenario.eq_ignore_ascii_case(name))
        .map(|(_, script)| (*script).to_string())
        .with_context(|| {
            format!(
                "unknown scenario `{name}`; available: {}",
                patpans::scenario_names()
            )
        })
}

fn format_event(event: Event) -> String {
    let marker = match event.edge {
        Edge::Press => '+',
        Edge::Release => '-',
    };
    format!("{}{marker}", event.key)
}

fn print_trace(sim: &SimBackend) {
    println!();
    println!("{:<14} output", "input");
    for (input, out) in sim.trace() {
        let output = if out.is_empty() {
            "(swallowed)".to_string()
        } else {
            out.iter()
                .map(|event| format_event(*event))
                .collect::<Vec<_>>()
                .join(" ")
        };
        println!("{:<14} {output}", format_event(*input));
    }
    println!();
    let state = if sim.final_enabled().unwrap_or(false) {
        "ON"
    } else {
        "OFF"
    };
    println!("snap tap enabled at the end: {state}");
}

fn cmd_check(path: &Path) -> Result<()> {
    let config = load_config(path)?;
    let toggle = config
        .toggle
        .map_or_else(|| "none".to_string(), |key| key.to_string());
    println!(
        "config: {} group(s), toggle: {toggle}, sticky: {}, tray: {}",
        config.groups.len(),
        config.sticky,
        config.tray
    );
    println!("config path: {}", path.display());
    for (index, group) in config.groups.iter().enumerate() {
        println!(
            "  group #{}: {}, {}",
            index + 1,
            group.keys[0],
            group.keys[1]
        );
    }
    #[cfg(target_os = "linux")]
    check_linux(&config);
    #[cfg(windows)]
    println!("windows: the low-level keyboard hook needs no extra permissions or drivers");
    Ok(())
}

#[cfg(target_os = "linux")]
fn check_linux(config: &Config) {
    let managed = managed_keys(config);
    let mut devices = 0;
    for (path, device) in evdev::enumerate() {
        let name = device.name().unwrap_or("unnamed");
        let hits = device.supported_keys().map_or(0, |supported| {
            supported
                .iter()
                .filter(|code| managed.iter().any(|key| key.linux_code == code.code()))
                .count()
        });
        let mark = if hits > 0 { "[managed]" } else { "         " };
        println!("  {mark} {} ({name})", path.display());
        devices += 1;
    }
    println!("  {devices} evdev device(s) visible");
    let uinput = if Path::new("/dev/uinput").exists() {
        "present"
    } else {
        "missing"
    };
    println!("  /dev/uinput: {uinput}");
}
