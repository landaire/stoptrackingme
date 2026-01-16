use arboard::Clipboard;
use clap::Parser;
use clap::Subcommand;
use reqwest::blocking::Client;
use reqwest::header::LOCATION;
use rootcause::Report;
use rootcause::prelude::ResultExt;
use std::borrow::Cow;
use std::time::Duration;
use tracing::Level;
use tracing::debug;
use tracing::debug_span;
use tracing::error;
use tracing::trace;
use tracing::warn;
use tracing_subscriber::fmt;
use url::Url;

use crate::matchers::Matcher;
use crate::matchers::ReplacementResult;
use crate::matchers::included_matchers;
use crate::matchers::load_matchers;

mod matchers;
/// Separate module defining the types used for matchers so that we can re-use them in the build.rs
mod matchers_types;

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
    /// Installs a service on the machine that will cause the application to be
    /// run automatically on login/system start
    InstallService,
    /// Uninstalls the system service
    UninstallService,
    /// Starts the system service
    StartService,
    /// Stops the system service
    StopService,
    /// Runs the application (default)
    #[default]
    Run,
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

    let matchers = if cfg!(debug_assertions) {
        // Load from the current directory
        load_matchers("matchers")?.leak()
    } else {
        included_matchers::get()
    };

    let mut http_client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        // Chrome for macOS reduced user-agent: https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/User-Agent
        // This is required because some site, like Reddit, will return a 403 if you use a user-agent it doesn't like
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36")
        .build()
        .context("failed to build HTTP client")?;

    let mut clipboard = Clipboard::new().unwrap();
    let mut last_content = clipboard.get_text().ok().map(ClipboardText);

    loop {
        let mut current = clipboard.get_text().ok().map(ClipboardText);
        if current != last_content {
            trace!("New clipboard text detected. Current: {current:?}, last: {last_content:?}");
            if let Some(ref mut new_clipboard_text) = current {
                match clean_clipboard_text(new_clipboard_text, &mut http_client, matchers) {
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
fn clean_clipboard_text(
    text: &ClipboardText,
    http_client: &mut Client,
    matchers: &[Matcher],
) -> Result<Option<ClipboardText>, Report> {
    let span = debug_span!("clean_clipboard_text");
    let _enter = span.enter();

    if cfg!(debug_assertions) {
        debug!("cleaning text: {text}")
    } else {
        debug!("cleaning text")
    }

    let mut text = Cow::Borrowed(&text.0);
    let mut final_result = Ok(None);
    let mut redirect_depth = 0;
    'url_loop: loop {
        let Ok(mut parsed_url) = Url::parse(text.as_ref()) else {
            // Not a URL
            return Ok(None);
        };

        let Some(host) = parsed_url.host_str().map(|host| host.to_owned()) else {
            // No valid host -- nothing to replace
            return Ok(None);
        };

        // Check to see if we can resolve a matcher
        for matcher in matchers {
            if !matcher.handles_host(&host) {
                continue;
            }

            debug!("{} matcher supports domain", &matcher.name);

            match matcher.run_replacements(&mut parsed_url) {
                ReplacementResult::Continue { modified } => {
                    debug!("Matcher requested a continue");

                    if modified {
                        final_result = Ok(Some(parsed_url.to_string()));
                    }
                    continue;
                }
                ReplacementResult::Stop => {
                    debug!("Matcher requested a stop");

                    final_result = Ok(Some(parsed_url.to_string()));
                    break;
                }
                ReplacementResult::RequestRedirect => {
                    debug!("Matcher requested a redirect");

                    if redirect_depth > 0 {
                        warn!("A prior matcher already requested a redirect. Nested redirects are to be ignored.");
                        // We aren't going to request any deeper
                        break;
                    }

                    let response =
                        http_client.head(parsed_url.clone()).send().context("failed to get redirect target")?;

                    if response.status().is_redirection() {
                        if let Some(location_header) = response.headers().get(LOCATION) {
                            text = Cow::Owned(
                                location_header
                                    .to_str()
                                    .context("failed to convert Location header to text")?
                                    .to_owned(),
                            );
                            final_result = Ok(Some(parsed_url.to_string()));

                            trace!("Got redirection location: {text}");

                            redirect_depth += 1;
                            // We need to run matchers again on this inner URL
                            continue 'url_loop;
                        } else {
                            warn!("Did not get a Location header in redirect response");
                            // We didn't have a location header?
                        }
                    } else {
                        warn!("Did not get a a redirect response");
                    }
                }
            }
        }

        // Avoiding infinite loops: all actions above
        // must take an explicit `continue` or `break`
        break;
    }

    final_result.map(|opt| opt.map(ClipboardText))
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
    }

    Ok(())
}
