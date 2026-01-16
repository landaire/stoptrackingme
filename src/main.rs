use arboard::Clipboard;
use clap::{Parser, Subcommand};
use reqwest::{Client, header::LOCATION};
use rootcause::{Report, prelude::ResultExt};
use std::{borrow::Cow, time::Duration};
use tracing::{Level, debug, debug_span, error, trace, warn};
use tracing_subscriber::fmt;
use url::Url;

use crate::matchers::{Matcher, ReplacementResult, included_matchers, load_matchers};

mod config;
mod matchers;

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
    /// Startsthe system service
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Report> {
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

    tracing::subscriber::set_global_default(subscriber)
        .expect("setting default tracing subscriber failed");

    let matchers = if cfg!(debug_assertions) {
        // Load from the current directory
        load_matchers("matchers")?.leak()
    } else {
        included_matchers::get()
    };

    let mut http_client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        // Chrome for macOS reduced user-agent: https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/User-Agent
        // This is required because some site, like Reddit, will return a 403 if you use a user-agent it doesn't like
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36")
        .build()
        .context("failed to build HTTP client")?;

    let mut clipboard = Clipboard::new().unwrap();
    let mut last_content = clipboard.get_text().ok();

    loop {
        let mut current = clipboard.get_text().ok();
        if current != last_content {
            trace!("New clipboard text detected. Current: {current:?}, last: {last_content:?}");
            if let Some(ref mut new_clipboard_text) = current {
                match clean_clipboard_text(new_clipboard_text, &mut http_client, matchers).await {
                    Ok(None) => {
                        // Not a URL, so don't update anything
                        debug!("Clipboard text not cleaned");
                    }
                    Ok(Some(new_url)) => {
                        // URL detected and updated
                        debug!("Cleaned URL: {new_url}");
                        if let Err(e) = clipboard.set_text(&new_url) {
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
async fn clean_clipboard_text(
    text: &str,
    http_client: &mut Client,
    matchers: &[Matcher],
) -> Result<Option<String>, Report> {
    let span = debug_span!("clean_clipboard_text");
    let _enter = span.enter();

    if cfg!(debug_assertions) {
        debug!("cleaning text: {text}")
    } else {
        debug!("cleaning text")
    }

    let mut text = Cow::Borrowed(text);
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
                ReplacementResult::Continue => {
                    debug!("Matcher requested a continue");
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
                        warn!(
                            "A prior matcher already requested a redirect. Nested redirects are to be ignored."
                        );
                        // We aren't going to request any deeper
                        break;
                    }

                    let response = http_client
                        .head(parsed_url.clone())
                        .send()
                        .await
                        .context("failed to get redirect target")?;

                    debug!("got response: {:?}", response);

                    if response.status().is_redirection() {
                        if let Some(location_header) = response.headers().get(LOCATION) {
                            text = Cow::Owned(
                                location_header
                                    .to_str()
                                    .context("failed to convert Location header to text")?
                                    .to_owned(),
                            );

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

    final_result
}

#[cfg(feature = "service")]
fn handle_command(command: Commands) -> Result<(), Report> {
    use service_manager::{
        RestartPolicy, ServiceInstallCtx, ServiceLabel, ServiceLevel, ServiceManager,
        ServiceStartCtx, ServiceUninstallCtx,
    };

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
                    restart_policy: RestartPolicy::OnFailure {
                        delay_secs: Some(10),
                    },
                })
                .context("failed to install service")?;

            println!(
                "Successfully installed service with label {SERVICE_NAME:?}. You can now start it with:"
            );
            println!("stoptrackingme start-service")
        }
        Commands::UninstallService => {
            manager
                .uninstall(ServiceUninstallCtx { label })
                .context("failed to uninstall service")?;

            println!("Successfully uninstalled service");
        }
        Commands::StartService => {
            manager
                .start(ServiceStartCtx { label })
                .context("failed to start service")
                .attach(SERVICE_NAME)?;

            println!("Successfully started service");
        }
        Commands::StopService => {
            use service_manager::ServiceStopCtx;

            manager
                .stop(ServiceStopCtx { label })
                .context("failed to stop service")
                .attach(SERVICE_NAME)?;

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
