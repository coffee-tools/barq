# Barq

Barq, ( بَرْق - meaning "lightning" in Arabic), is a Rust plugin for Core Lightning designed to provide a simple and extensible interface for paying invoices on the Lightning Network.

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

A user guide for this plugin can be found here: https://github.com/tareknaser/barq/blob/main/USER_GUIDE.md

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

## Contributions

Contributions to Barq are highly appreciated. Please follow the guidelines in [`CONTRIBUTING.md`](./CONTRIBUTING.md) for details on linting, formatting, testing, and commit message conventions.
