# stoptrackingme

Removes sharing IDs and other types of link trackers from URLs.

This is a simple CLI application which runs in the background and monitors the user clipboard for URLs containing undesirable tracking IDs or query params. When a link is modified to remove query params or tracking identifiers, it's then updated in your clipboard automatically.

## Usage

```
$ stoptrackingme --help

Monitor the system clipboard for URLs and remove tracking IDs

Usage: stoptrackingme [COMMAND]

Commands:
  install-service    Installs a service on the machine that will cause the application to be run automatically on login/system start
  uninstall-service  Uninstalls the system service
  start-service      Startsthe system service
  stop-service       Stops the system service
  run                Runs the application (default)
  help               Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

Use one of the options below for installation. If you want to just give it a try with nix, you can also run:

```
nix run github:landaire/stoptrackingme
```

## Installation

### From Source

```
git clone https://github.com/landaire/stoptrackingme.git
cd stoptrackingme
cargo install --locked . # or cargo {run,build} --release
```

### cargo

```
cargo install --locked stoptrackingme
```

## Features

- [x] Drop params with simple share IDs or source IDs (`?share_id=<ID>&utm_whatever=iphone`)
- [x] Full replacement URLs that require following an HTTP redirect (e.g. `https://www.reddit.com/r/SUBREDDIT/s/SHAREDID`)
- [x] Global modifiers for annoying URL query params (`?utm_*`)
- [x] System service management

May be nice to have:

- Anonymous HTTP requests (Tor, proxy) to obtain the URL without the share ID for those that cannot be resolved in a simple manner. You could still be tied to a share ID based on IP address, even when not logged in to an account.

## Supported Sites

Currently supported websites can be found in the [matchers](/matchers) directory.
