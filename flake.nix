{
  description = "Integration Testing";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    naersk.url = "github:nix-community/naersk";
  };

  outputs = { self, nixpkgs, flake-utils, naersk }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # Core Lightning from nixpkgs (v26.06.9 at the locked revision):
        # barq's `pay` hook needs v25.12 or later (rpc_command filters) and
        # BOLT12 self-payments need injectpaymentonion (v24.11).
        clightning = pkgs.clightning;
        # Our integration tests required the cln and bitcoind
        # so in this variable we declare everything that we need
        # for bitcoin and cln
        cln-env-shell = [ clightning pkgs.bitcoind ];

        # build rust application :)
        naersk' = pkgs.callPackage naersk { };
      in
      rec {
        # FIXME: will be good to have this formatting also the rust code
        formatter = pkgs.nixpkgs-fmt;

        devShell = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; [
            gnumake

            rustc
            cargo
            rustfmt
            # The test dependencies have no wheels for newer Pythons yet.
            python312
            poetry

            openssl
            openssl.dev
          ] ++ cln-env-shell;
          shellHook = ''
            export HOST_CC=gcc
            export RUST_BACKTRACE=1
            export PYTHON_KEYRING_BACKEND=keyring.backends.null.Keyring

            # In a subshell so a failing step never leaves the shell in tests/.
            # Poetry 2 dropped `lock --no-update`: `install` uses the lock file.
            (cd tests && poetry env use "$(which python)" && poetry install -vvv)
          '';
        };
      }
    );
}
