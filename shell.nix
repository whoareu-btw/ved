# For people who do not use flakes:  nix-shell
{
  pkgs ? import <nixpkgs> { },
}:
pkgs.mkShell {
  packages = with pkgs; [
    cargo
    rustc
    clippy
    rustfmt
    rust-analyzer
    python3
  ];
  RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
}
