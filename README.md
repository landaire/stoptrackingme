# stoptrackingme

Removes sharing IDs and other types of link trackers from URLs

## Features

- [x] Drop params with simple share IDs (`?share_id=<ID>`)
- [x] Full replacement URLs that require following an HTTP redirect (e.g. `https://www.reddit.com/r/SUBREDDIT/s/SHAREDID`)
- [x] Global modifiers for annoying URL query params (`?utm_*`)

May be nice to have:

- Anonymous HTTP requests (Tor, proxy) to obtain the URL without the share ID for those that cannot be resolved in a simple manner. You could still be tied to a share ID based on IP address, even when not logged in to an account.

## Supported Sites

Currently supported websites can be found in the [matchers](/matchers) directory.
