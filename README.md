# Barq

Barq, ( بَرْق - meaning "lightning" in Arabic), is a Rust plugin for Core Lightning designed to provide a simple and extensible interface for paying invoices on the Lightning Network.

It pays BOLT11 and BOLT12 invoices by finding the path with [LDK](https://lightningdevkit.org) (router and probabilistic scorer) over a network graph fed by LDK rapid gossip sync and the Core Lightning `gossip_store`, and completes the payment with the Core Lightning `createonion`/`sendonion` API.

## Project Structure

Barq is organized into multiple crates:

| Crate Name    | Purpose                                                        |
| ------------- | -------------------------------------------------------------- |
| `barq-common` | Contains core logic and implements the strategy design pattern |
| `barq-plugin` | Plugin with RPC commands for interacting with Core Lightning   |

## Strategy Design Pattern

The strategy design pattern is used to create a flexible routing mechanism. This allows for different routing strategies to be implemented and selected at runtime, which enhances the modularity and maintainability of the codebase.

Each strategy follows a common interface, making it easy to add new strategies without changing existing code for the plugin.

## User Guide

A user guide for this plugin can be found here: [USER_GUIDE.md](./USER_GUIDE.md)

## Integration Testing

To run integration tests, we use `nix`. After [installing nix](https://nixos.org/download), run the following commands:

```shell
nix develop
make check
```

Alternatively, you can run:

```shell
nix develop --command bash -c 'make check'
```

To run the integration tests against a specific Core Lightning build, put
its `bin` directory first in `PATH`. The suite is verified on v25.09 and
v26.06.9. Set `RUST=0` when that build has no Rust plugins (no `cln-grpc`):

```shell
cd tests
RUST=0 PATH=/path/to/cln/bin:$PATH poetry run pytest . -v
```

## Contributions

Contributions to Barq are highly appreciated. Please follow the guidelines in [`CONTRIBUTING.md`](./CONTRIBUTING.md) for details on linting, formatting, testing, and commit message conventions.
