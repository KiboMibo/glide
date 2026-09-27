// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::borrow::Borrow;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use glide_wm::actor::server::{
    self, AsciiEscaped, PROTOCOL_VERSION, Request, Response, ServiceRequest,
};
use glide_wm::actor::updater::{UpdaterState, install};
use glide_wm::config::{Config, config_path};
use glide_wm::sys::bundle::{self, BundleError};
use glide_wm::sys::message_port::{RemoteMessagePort, RemotePortCreateError, SendError};
use notify::RecursiveMode;
use notify_debouncer_mini::new_debouncer;

const TIMEOUT: Duration = Duration::from_millis(1000);

/// Client to control a running Glide server.
#[derive(Parser)]
#[command(version, name = "glide")]
struct Opt {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Clone)]
enum Command {
    /// Launch Glide.
    Launch(CmdLaunch),
    #[command(subcommand)]
    Service(CmdService),
    #[command()]
    Ping(CmdPing),
    #[command()]
    Config(CmdConfig),
    /// Pause window management on all spaces.
    Pause,
    /// Resume window management after pausing.
    Resume,
    /// Show the versions of the CLI and the running server.
    Version,
    #[command(subcommand)]
    Update(CmdUpdateGlide),
}

/// Check for and install updates to Glide.
#[derive(Subcommand, Clone)]
enum CmdUpdateGlide {
    /// Check whether an update is available.
    Check,
    /// Install the available update and restart Glide.
    Install,
    /// Replace an installed Glide with the bundle this command runs from, then
    /// launch it.
    ///
    /// Glide runs this from a downloaded update. Its arguments must stay
    /// compatible with every older version that might run it.
    #[command(hide = true)]
    Apply(CmdApply),
}

#[derive(Parser, Clone)]
struct CmdApply {
    /// The installed bundle to replace.
    #[arg(long)]
    target: PathBuf,

    /// Wait for this process to exit before replacing the bundle.
    #[arg(long)]
    wait_pid: Option<i32>,

    /// Arguments to launch the new version with.
    #[arg(last = true)]
    args: Vec<OsString>,
}

/// Manage Glide as a system service.
#[derive(Subcommand, Clone)]
enum CmdService {
    /// Add Glide to login items.
    Install,
    /// Remove Glide from login items.
    Uninstall,
}

/// Checks if the server is running.
#[derive(Parser, Clone)]
struct CmdPing {
    msg: Option<String>,
}

/// Launch Glide with optional configuration.
#[derive(Parser, Clone)]
struct CmdLaunch {
    /// Path to a custom config file.
    #[arg(long, short)]
    config: Option<PathBuf>,

    /// Restore the layout and enabled spaces saved when Glide last exited in
    /// this login session. This is the default unless auto_restore is off in
    /// the config.
    ///
    /// Also restores a layout saved by an older version of Glide.
    #[arg(long, overrides_with = "no_restore")]
    restore: bool,

    /// Start with a fresh layout instead of restoring the saved one.
    #[arg(long, overrides_with = "restore")]
    no_restore: bool,
}

/// Manage server config.
#[derive(Parser, Clone)]
struct CmdConfig {
    /// Path to a custom config file.
    #[arg(long, short, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    action: ConfigSubcommand,
}

#[derive(Subcommand, Clone)]
enum ConfigSubcommand {
    /// Read the config file and update the config on the running server.
    Update(CmdUpdate),
    /// Check the config file for errors.
    Verify,
}

/// Updates the server config by parsing the config file on disk.
///
/// The config file lives at ~/.glide.toml.
#[derive(Parser, Clone)]
struct CmdUpdate {
    /// Watch for config changes, continuously updating the file.
    #[arg(long)]
    watch: bool,
}

fn main() -> Result<(), anyhow::Error> {
    let opt: Opt = Parser::parse();

    // Not all commands require a client, so defer it.
    let make_client = || Client::new().context("Could not find server");

    match opt.command {
        Command::Launch(CmdLaunch { config, restore, no_restore }) => {
            let restore = match (restore, no_restore) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            };
            launch(config, restore)?
        }
        Command::Service(req) => {
            let (req, verb) = match req {
                CmdService::Install => (ServiceRequest::Install, "registered"),
                CmdService::Uninstall => (ServiceRequest::Uninstall, "unregistered"),
            };
            let response = make_client()?.send(Request::Service(req))?;
            match response {
                Response::Success => println!("Glide was {verb} as a service"),
                Response::Error(e) => bail!("{e}"),
                _ => bail!("Unexpected response"),
            }
        }
        Command::Ping(send) => {
            let response = make_client()?.send(Request::Ping(send.msg.unwrap_or_default()))?;
            match response {
                Response::Pong(data) => eprintln!("Got response {data}"),
                _ => bail!("Unexpected response"),
            }
        }
        Command::Version => version()?,
        Command::Update(CmdUpdateGlide::Check) => match check_for_update(&make_client()?)? {
            Some(version) => println!("Glide {version} is available"),
            None => println!("Glide is up to date"),
        },
        Command::Update(CmdUpdateGlide::Install) => install_update(&make_client()?)?,
        Command::Update(CmdUpdateGlide::Apply(cmd)) => apply_update(cmd)?,
        Command::Pause => set_enabled(make_client()?, false)?,
        Command::Resume => set_enabled(make_client()?, true)?,
        Command::Config(CmdConfig {
            config,
            action: ConfigSubcommand::Update(CmdUpdate { watch }),
        }) => {
            let mut client = make_client()?;
            let mut update_config = || {
                if !config.as_deref().unwrap_or(&config_path()).exists() {
                    eprintln!("Warning: Config file missing; will load defaults");
                }
                let config = match Config::load(config.as_deref()) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("{e}\n");
                        return;
                    }
                };
                let request = Request::UpdateConfig(config);
                loop {
                    match client.send(&request) {
                        Ok(Response::Success) => eprintln!("config updated"),
                        Ok(resp) => eprintln!("Unexpected response: {resp:?}"),
                        Err(ClientError::SendError(SendError::InvalidPort)) => {
                            eprintln!("Could not send to server; will attempt reconnect");
                            client = make_client().unwrap();
                            continue;
                        }
                        Err(e) => eprintln!("Error: {e}"),
                    }
                    break;
                }
            };
            if watch {
                let (tx, rx) = mpsc::channel();
                let mut debouncer = new_debouncer(Duration::from_millis(50), tx)?;
                debouncer.watcher().watch(&config_path(), RecursiveMode::NonRecursive)?;
                update_config();
                for event in rx {
                    event?;
                    update_config();
                }
            } else {
                update_config();
            }
        }
        Command::Config(CmdConfig {
            config,
            action: ConfigSubcommand::Verify,
        }) => {
            if !config.as_deref().unwrap_or(&config_path()).exists() {
                bail!("Config file missing");
            }
            if let Err(e) = Config::load(config.as_deref()) {
                eprintln!("{e}");
                std::process::exit(1);
            }
            eprintln!("config ok");
        }
    }

    Ok(())
}

/// How long to wait for the new version to start after the old one exits.
const RESTART_TIMEOUT: Duration = Duration::from_secs(30);
/// How many status requests in a row can fail while waiting for the server to
/// exit before giving up.
const MAX_EXIT_FAILURES: u32 = 5;

fn update_status(client: &Client) -> Result<UpdaterState, anyhow::Error> {
    match client.send(Request::UpdateStatus) {
        Ok(Response::UpdateStatus(status)) => Ok(status),
        Ok(resp) => bail!("Unexpected response: {resp:?}"),
        // Servers that can't update respond with an empty message.
        Err(ClientError::SerializationError(_)) => {
            bail!("The running version of Glide can't update itself")
        }
        Err(e) => Err(e.into()),
    }
}

fn expect_success(response: Response) -> Result<(), anyhow::Error> {
    match response {
        Response::Success => Ok(()),
        Response::Error(e) => bail!("{e}"),
        resp => bail!("Unexpected response: {resp:?}"),
    }
}

/// Call `f` until it returns a value.
///
/// We need this because CFMessagePort is synchronous, and we don't want to
/// block other messages (or the main thread) during update operations.
fn poll<T>(mut f: impl FnMut() -> Result<Option<T>, anyhow::Error>) -> Result<T, anyhow::Error> {
    loop {
        if let Some(value) = f()? {
            return Ok(value);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Check for an update, returning the version available if there is one.
fn check_for_update(client: &Client) -> Result<Option<String>, anyhow::Error> {
    let before = update_status(client)?;
    if let Some(reason) = before.disabled {
        bail!("Updates are disabled: {reason}");
    }
    expect_success(client.send(Request::CheckForUpdate)?)?;
    let status = poll(|| {
        let status = update_status(client)?;
        Ok((status.checks > before.checks || status.disabled.is_some()).then_some(status))
    })?;
    if let Some(reason) = status.disabled {
        bail!("Updates are disabled: {reason}");
    }
    if let Some(e) = status.error {
        bail!("Checking for updates failed: {e}");
    }
    Ok(status.available)
}

fn install_update(client: &Client) -> Result<(), anyhow::Error> {
    let Some(version) = check_for_update(client)? else {
        println!("Glide is up to date");
        return Ok(());
    };
    println!("Installing Glide {version}");
    expect_success(client.send(Request::InstallUpdate)?)?;
    // Wait for the server to exit, which it does once it hands off to the new
    // version. A request in flight as it exits can fail in other ways before
    // its port is seen to be gone, so retry those a few times.
    let mut failures = 0;
    poll(|| match update_status(client) {
        Ok(status) if status.installing => {
            failures = 0;
            Ok(None)
        }
        Ok(status) => bail!("Installing failed: {}", status.error.unwrap_or_default()),
        Err(e) if is_server_gone(&e) => Ok(Some(())),
        Err(_) if failures < MAX_EXIT_FAILURES => {
            failures += 1;
            Ok(None)
        }
        Err(e) => Err(e),
    })?;
    println!("Waiting for Glide to restart");
    let start = std::time::Instant::now();
    let running = poll(|| {
        if start.elapsed() > RESTART_TIMEOUT {
            bail!("Glide did not restart. See ~/.glide/update.log for details.");
        }
        let Ok(client) = Client::new() else { return Ok(None) };
        let hello = Request::Hello {
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol: PROTOCOL_VERSION,
        };
        match client.send(hello) {
            Ok(Response::Hello { server_version, .. }) => Ok(Some(server_version)),
            _ => Ok(None),
        }
    })?;
    if running != version {
        bail!(
            "Glide restarted with version {running} instead of {version}. \
             See ~/.glide/update.log for details."
        );
    }
    println!("Glide restarted with version {version}");
    Ok(())
}

fn is_server_gone(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<ClientError>(),
        Some(ClientError::SendError(SendError::InvalidPort))
    )
}

fn apply_update(cmd: CmdApply) -> Result<(), anyhow::Error> {
    let source = match bundle::glide_bundle() {
        Ok(bundle) => PathBuf::from(bundle.bundlePath().to_string()),
        Err(_) => bail!("Not running from a Glide bundle"),
    };
    eprintln!(
        "Installing Glide {} from {} to {}",
        env!("CARGO_PKG_VERSION"),
        source.display(),
        cmd.target.display()
    );
    if let Some(pid) = cmd.wait_pid {
        install::wait_for_exit(pid)?;
    }
    let result = install::replace_bundle(&source, &cmd.target);
    match &result {
        Ok(()) => eprintln!("Installed; launching {}", cmd.target.display()),
        Err(e) => eprintln!("Install failed; relaunching the existing version: {e:#}"),
    }
    install::launch(&cmd.target, &cmd.args)?;
    result
}

fn version() -> Result<(), anyhow::Error> {
    println!(
        "client: {} (protocol {PROTOCOL_VERSION})",
        env!("CARGO_PKG_VERSION")
    );
    let server = match Client::new() {
        Err(_) => "not running".to_owned(),
        Ok(client) => {
            let hello = Request::Hello {
                client_version: env!("CARGO_PKG_VERSION").to_owned(),
                protocol: PROTOCOL_VERSION,
            };
            match client.send(hello) {
                Ok(Response::Hello { server_version, protocol }) => {
                    format!("{server_version} (protocol {protocol})")
                }
                // Servers from before the Hello request respond with an empty
                // message.
                Err(ClientError::SerializationError(_)) => "unknown (older version)".to_owned(),
                Ok(resp) => bail!("Unexpected response: {resp:?}"),
                Err(e) => return Err(e.into()),
            }
        }
    };
    println!("server: {server}");
    Ok(())
}

fn set_enabled(client: Client, enabled: bool) -> Result<(), anyhow::Error> {
    match client.send(Request::SetEnabled(enabled))? {
        Response::Success => {
            println!("Glide {}", if enabled { "resumed" } else { "paused" });
            Ok(())
        }
        Response::Error(e) => bail!("{e}"),
        _ => bail!("Unexpected response"),
    }
}

fn launch(config: Option<PathBuf>, restore: Option<bool>) -> Result<(), anyhow::Error> {
    match bundle::glide_bundle() {
        Err(BundleError::NotInBundle) => bail!(
            "Not running in a bundle.
                \n\
                To run glide from the command line, use `cargo run` or start glide_server directly."
        ),
        Err(BundleError::BundleNotGlide { identifier }) => {
            bail!("Don't recognize bundle identifier {identifier}")
        }
        Ok(bundle) => {
            let config_result = Config::load(config.as_deref());
            if let Err(e) = config_result {
                bail!("Config is invalid; refusing to launch:\n{e}");
            }
            if Client::new().is_ok() {
                bail!(
                    "Glide appears to be running already.
                        \n\
                        Tip: The default key binding to exit Glide is Alt+Shift+E."
                );
            }
            let mut args = Vec::new();
            if let Some(path) = &config {
                args.push("--config".into());
                args.push(path.canonicalize()?.into_os_string());
            }
            match restore {
                Some(true) => args.push("--restore".into()),
                Some(false) => args.push("--no-restore".into()),
                None => {}
            }
            bundle::launch(&bundle, &args)?;
            eprintln!(
                "Glide is starting.
                    \n\
                    Tip: Use Alt+Z to start managing the current space.\n\
                    Tip: Use Alt+Shift+E to exit Glide."
            );
            Ok(())
        }
    }
}

struct Client {
    port: RemoteMessagePort,
}

#[derive(thiserror::Error, Debug)]
enum ClientError {
    #[error("Serialization error")]
    SerializationError(#[source] anyhow::Error),
    #[error("Sending message failed")]
    SendError(#[source] SendError),
}

impl Client {
    fn new() -> Result<Self, RemotePortCreateError> {
        Ok(Self {
            port: RemoteMessagePort::new(server::PORT_NAME)?,
        })
    }

    fn send(&self, req: impl Borrow<Request>) -> Result<Response, ClientError> {
        let msg = ron::ser::to_string(req.borrow())
            .context("Serializing message failed")
            .map_err(ClientError::SerializationError)?;
        let resp = self
            .port
            .send_message(0, msg.as_bytes(), TIMEOUT)
            .map_err(ClientError::SendError)?;
        let response = ron::de::from_bytes(&resp)
            .with_context(|| format!("Response: \"{}\"", AsciiEscaped(&resp)))
            .context("Deserializing response failed")
            .map_err(ClientError::SerializationError)?;
        Ok(response)
    }
}
