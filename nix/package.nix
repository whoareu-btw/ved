# The ved package. Used by flake.nix and default.nix; callPackage-able on its own:
#   pkgs.callPackage ./nix/package.nix { }
{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "ved";
  version = (builtins.fromTOML (builtins.readFile ../Cargo.toml)).package.version;

  # only what the build needs, so editing the README or the tests does not rebuild the package
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
    ];
  };

  # ved has no dependencies, so the lock file only pins the package itself
  cargoLock.lockFile = ../Cargo.lock;

  # The tests drive the real binary on a pseudo-terminal and start scripted language servers; run them
  # with `cargo test` in the dev shell (`nix develop` or `nix-shell`) instead of in the build sandbox.
  doCheck = false;

  meta = {
    description = "A small, dependency-free vim-style terminal editor with built-in LSP and an embedded terminal";
    license = lib.licenses.mit;
    mainProgram = "ved";
    platforms = lib.platforms.unix;
  };
}
