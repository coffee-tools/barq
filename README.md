# Barq

Barq, ( بَرْق - meaning "lightning" in Arabic), is a Rust plugin for Core Lightning designed to provide a simple and extensible interface for paying invoices on the Lightning Network.

## Project Structure

Barq is organized into multiple crates:

| Crate Name    | Purpose                                                        |
| ------------- | -------------------------------------------------------------- |
| `barq-common` | Contains core logic and implements the strategy design pattern |
| `barq-plugin` | Plugin with RPC commands for interacting with Core Lightning   |

## Integration Testing

To run integration testing we use `nix`, so after you [install nix](https://nixos.org/download), and
then run the following commands

``` shell
nix develop
make check
```

or you can use `nix develop --command bash -c 'make check'`
