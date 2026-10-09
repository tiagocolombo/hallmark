# `hall` without flakes and without the private `dev` input: only nixpkgs.
#
#   nix-build nix                    # ./result/bin/hall
#   nix profile install --file nix   # or: nix-env -f nix -i
{
  pkgs ? import <nixpkgs> { },
}:

pkgs.callPackage ./package.nix { }
