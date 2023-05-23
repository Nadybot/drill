# drill

Drill is a simple, no-bullshit HTTP tunnel that just works. It only needs a single port and expects to run behind a reverse proxy such as nginx.

## Features

- Tiny binaries
- Concurrent and multi-threaded
- Supports multiplexing
- Websocket support

## Downloading

Currently, we do not provide prebuilts. You will have to compile it from source yourself.

## Running the server

The most simple server configuration would look like this:

`$ drill-server --auth anonymous --subdomain-strategy random --websocket-host mydomain.com --domain mydomain.com --description "Welcome to my Drill instance!" -p 7777`

This would run a public instance which hands out random subdomains (in the form of `asdf.mydomain.com`) with the control websocket running at `mydomain.com`.

The server supports several different authentication backends (via `--auth`):

- `ao` requires clients to authorize using a token they receive in a tell in Anarchy Online (this is probably not wanted by anyone except us)
- `private` requires `--token-file` to be set and verifies tokens against the list of tokens in the file
- `anonymous` accepts any tokens and is the least secure
- `dynamic` requires `--auth-backend` and will send a HTTP GET request with the query parameters `token` and `desired_subdomain` to the URL specified. The backend can grant access with a 2xx response and the subdomain for the client as plaintext in the body or reject it with any other status code

The server supports three subdomain strategies:

- `ao-character` can be used with the `ao` authentication backend
- `client-choice` allows clients to choose the subdomain they want
- `random` means the server will randomize the subdomains granted

Obviously, `dynamic` auth ignores this setting.

## Running the client

`$ drill-client -p 8080 -s subdomain -d mydomain.com`

This will connect to the Drill instance at `mydomain.com`, attempt to claim `subdomain.mydomain.com` if possible and tunnel traffic to port 8080 locally.
