# For people who do not use flakes:  nix-build   or   nix-env -f . -i
{
  pkgs ? import <nixpkgs> { },
}:
pkgs.callPackage ./nix/package.nix { }
