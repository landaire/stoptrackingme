{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    flake-utils,
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {inherit system overlays;};
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = ["rust-src" "rust-analyzer"];
        };
        darwinDeps = pkgs.lib.optionals pkgs.stdenv.isDarwin [
          pkgs.apple-sdk_15
          (pkgs.darwinMinVersionHook "10.13")
        ];
      in {
        devShells.default = pkgs.mkShell {
          buildInputs = [rust] ++ darwinDeps;
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "stoptrackingme";
          version = "0.1.3";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          buildInputs = darwinDeps;
        };
      }
    );
}
