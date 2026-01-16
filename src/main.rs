use arboard::Clipboard;
use clap::Parser;
use clap::Subcommand;
use clearurls::UrlCleaner;
use rootcause::Report;
use rootcause::prelude::ResultExt;
use std::path::PathBuf;
use std::time::Duration;
use tracing::Level;
use tracing::debug;
use tracing::debug_span;
use tracing::error;
use tracing::trace;
use tracing_subscriber::fmt;
use url::Url;

const CLIPBOARD_POLLING_RATE: Duration = Duration::from_millis(500);

/// Monitor the system clipboard for URLs and remove tracking IDs
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[cfg(feature = "service")]
#[derive(Debug, Default, Eq, PartialEq, Subcommand)]
enum Commands {
    /// Runs the application (default)
    #[default]
    Run,
    /// Installs a service on the machine that will cause the application to be
    /// run automatically on login/system start
    InstallService,
    /// Uninstalls the system service
    UninstallService,
    /// Starts the system service
    StartService,
    /// Stops the system service
    StopService,
    /// Prints the expected config path
    ConfigPath,
}

#[cfg(not(feature = "service"))]
#[derive(Debug, Default, Eq, PartialEq, Subcommand)]
enum Commands {
    /// Runs the application (default)
    #[default]
    Run,
}

#[derive(PartialEq, Eq)]
struct ClipboardText(String);

impl std::fmt::Display for ClipboardText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if cfg!(debug_assertions) { f.write_str(&self.0) } else { f.write_str("[redacted]") }
    }
}

impl std::fmt::Debug for ClipboardText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut output = f.debug_tuple("ClipboardText");
        if cfg!(debug_assertions) {
            output.field(&self.0);
        } else {
            output.field(&"[redacted]");
        }

        output.finish()
    }
}

fn main() -> Result<(), Report> {
    let args = Args::parse();
    let command = args.command.unwrap_or_default();
    if command != Commands::Run {
        return handle_command(command);
    }

    let subscriber = if cfg!(debug_assertions) {
        fmt().pretty().with_max_level(Level::TRACE).finish()
    } else {
        fmt().pretty().with_max_level(Level::DEBUG).finish()
    };

    tracing::subscriber::set_global_default(subscriber).expect("setting default tracing subscriber failed");

    let cleaner = if let Some(data_path) = config_path().map(|path| path.join("data.json"))
        && data_path.exists()
    {
        UrlCleaner::from_rules_path(&data_path)
            .context("failed to load user-defined UrlCleaner data")
            .attach_with(move || format!("{data_path:?}"))?
    } else {
        UrlCleaner::from_embedded_rules().context("failed to load UrlCleaner")?
    };

    let mut clipboard = Clipboard::new().unwrap();
    let mut last_content = clipboard.get_text().ok().map(ClipboardText);

    loop {
        let mut current = clipboard.get_text().ok().map(ClipboardText);
        if current != last_content {
            trace!("New clipboard text detected. Current: {current:?}, last: {last_content:?}");
            if let Some(ref mut new_clipboard_text) = current {
                match clean_clipboard_text(new_clipboard_text, &cleaner) {
                    Ok(None) => {
                        // Not a URL, so don't update anything
                        debug!("Clipboard text not cleaned");
                    }
                    Ok(Some(new_url)) => {
                        // URL detected and updated
                        debug!("Cleaned URL: {new_url}");
                        if let Err(e) = clipboard.set_text(&new_url.0) {
                            error!("could not set clipboard text: {e:?}");
                        } else {
                            *new_clipboard_text = new_url;
                        }

                        debug!("new clipboard text: {new_clipboard_text:?}");
                    }
                    Err(e) => {
                        error!("could not clean URL: {e:?}");
                    }
                }
            }
            last_content = current;
        }

        std::thread::sleep(CLIPBOARD_POLLING_RATE);
    }
}

/// Attempts to parse a URL and clean it
fn clean_clipboard_text(text: &ClipboardText, cleaner: &UrlCleaner) -> Result<Option<ClipboardText>, Report> {
    let span = debug_span!("clean_clipboard_text");
    let _enter = span.enter();

    debug!("cleaning text: {text}");

    let Ok(parsed_url) = Url::parse(text.0.as_ref()) else {
        // Not a URL
        return Ok(None);
    };

    let cleaned = cleaner.clear_single_url(&parsed_url).context("failed to clean URL")?;

    if cleaned.as_ref() != &parsed_url { Ok(Some(ClipboardText(cleaned.to_string()))) } else { Ok(None) }
}

#[cfg(feature = "service")]
fn handle_command(command: Commands) -> Result<(), Report> {
    use service_manager::RestartPolicy;
    use service_manager::ServiceInstallCtx;
    use service_manager::ServiceLabel;
    use service_manager::ServiceLevel;
    use service_manager::ServiceManager;
    use service_manager::ServiceStartCtx;
    use service_manager::ServiceUninstallCtx;

    const SERVICE_NAME: &str = "net.landaire.stoptrackingme";
    const TARGET_SERVICE_LEVEL: ServiceLevel = ServiceLevel::User;
    let label: ServiceLabel = SERVICE_NAME.parse().expect("invalid ServiceLabel");
    let mut manager = <dyn ServiceManager>::native().expect("Failed to detect management platform");
    manager
        .set_level(TARGET_SERVICE_LEVEL)
        .context("failed to set ServiceManager's level")
        .attach_with(|| format!("level: {:?}", TARGET_SERVICE_LEVEL))?;

    match command {
        Commands::InstallService => {
            manager
                .install(ServiceInstallCtx {
                    label,
                    program: std::env::current_exe().expect("failed to get current program path"),
                    args: vec!["run".into()],
                    contents: None,
                    username: None,
                    working_directory: None,
                    environment: None,
                    autostart: true,
                    restart_policy: RestartPolicy::OnFailure { delay_secs: Some(10) },
                })
                .context("failed to install service")?;

            println!("Successfully installed service with label {SERVICE_NAME:?}. You can now start it with:");
            println!("stoptrackingme start-service")
        }
        Commands::UninstallService => {
            manager.uninstall(ServiceUninstallCtx { label }).context("failed to uninstall service")?;

            println!("Successfully uninstalled service");
        }
        Commands::StartService => {
            manager.start(ServiceStartCtx { label }).context("failed to start service").attach(SERVICE_NAME)?;

            println!("Successfully started service");
        }
        Commands::StopService => {
            use service_manager::ServiceStopCtx;

            manager.stop(ServiceStopCtx { label }).context("failed to stop service").attach(SERVICE_NAME)?;

            println!("Successfully stopped service");
        }
        Commands::Run => {
            unreachable!("Run command should be the default command and handled in main()")
        }
        Commands::ConfigPath => {
            if let Some(path) = config_path() {
                println!("Config path: {:?}", path)
            } else {
                println!("Could not resolve config path?")
            }
        }
    }

    Ok(())
}

#[cfg(not(feature = "service"))]
#[allow(unreachable_code)]
fn handle_command(command: Commands) -> Result<(), Report> {
    match command {
        Commands::Run => {
            unreachable!("Run command should be the default command and handled in main()")
        }
        Commands::ConfigPath => {
            if let Some(path) = config_path() {
                println!("Config path: {:?}", path)
            } else {
                println!("Could not resolve config path?")
            }
        }
    }

    Ok(())
}

fn config_path() -> Option<PathBuf> {
    dirs::config_local_dir().map(|path| path.join("stoptrackingme"))
}
