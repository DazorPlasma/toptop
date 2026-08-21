{
  description = "modern CLI system monitor";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    nixpkgs,
    flake-utils,
    rust-overlay,
    ...
  }: let
    overlay = final: prev: {
      toptop = final.callPackage ./package.nix {};
    };
  in
    {
      overlays.default = overlay;
    }
    // flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        toptop = rustPlatform.buildRustPackage {
          pname = "toptop";
          version = (pkgs.lib.importTOML ./Cargo.toml).package.version;

          src = pkgs.lib.cleanSource ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          meta = with pkgs.lib; {
            description = "modern CLI system monitor";
            homepage = "https://github.com/DazorPlasma/toptop";
            license = licenses.gpl3Plus;
            maintainers = [];
            mainProgram = "toptop";
            platforms = platforms.linux;
          };
        };
      in {
        packages = {
          default = toptop;
          inherit toptop;
        };

        apps.default = flake-utils.lib.mkApp {
          drv = toptop;
        };

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rustToolchain
            pkg-config
            openssl
          ];

          env = {
            RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
          };
        };
      }
    );
}
