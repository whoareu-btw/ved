{
  description = "ved: a small vim-style terminal editor with built-in LSP and an embedded terminal";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin" # nixpkgs 26.11 dropped x86_64-darwin
      ];
      forAllSystems =
        f: nixpkgs.lib.genAttrs systems (system: f system nixpkgs.legacyPackages.${system});
    in
    {
      # nix build           ->  ./result/bin/ved
      # nix run . -- file   ->  run it without installing
      packages = forAllSystems (
        system: pkgs: rec {
          ved = pkgs.callPackage ./nix/package.nix { };
          default = ved;
        }
      );

      apps = forAllSystems (
        system: pkgs: {
          default = {
            type = "app";
            program = "${self.packages.${system}.default}/bin/ved";
          };
        }
      );

      # nix develop         ->  cargo, rust-analyzer, clippy, rustfmt and python3 (for the test suite)
      devShells = forAllSystems (
        system: pkgs: {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
              rust-analyzer
              python3
            ];
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
        }
      );

      # for NixOS / home-manager:  nixpkgs.overlays = [ ved.overlays.default ];  then  pkgs.ved
      overlays.default = final: prev: {
        ved = final.callPackage ./nix/package.nix { };
      };
    };
}
