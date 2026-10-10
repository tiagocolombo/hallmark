{
  description = "Hallmark: the dev shell (declared in rig.toml) and the `hall` package";

  # SSH, not github:owner/repo — the repository is private and Nix's GitHub
  # fetcher calls the API unauthenticated, so it would 404. This uses the
  # SSH key git already has.
  inputs.rig.url = "git+ssh://git@github.com/tiagocolombo/rig";

  # The dev shell, services and apps come from `rig.lib.mkProject` exactly as
  # before. On top of them: the `hall` package (nix/package.nix), built with
  # the nixpkgs `rig` already pins, so there is one nixpkgs in the lock.
  #
  # Evaluating any output still fetches the private `rig` input. Without
  # access to it, build with nix/default.nix, which needs only nixpkgs.
  # See docs/decisions/0023-packaging-and-releases.md.
  outputs =
    { rig, ... }:
    let
      inherit (rig.inputs) nixpkgs;
      inherit (nixpkgs) lib;
      project = rig.lib.mkProject ./rig.toml;
      systems = builtins.attrNames project.devShells;
      hall = lib.genAttrs systems (
        system: nixpkgs.legacyPackages.${system}.callPackage ./nix/package.nix { }
      );
      app = system: {
        type = "app";
        program = lib.getExe hall.${system};
        meta.description = "hall, the Hallmark CLI";
      };
    in
    lib.recursiveUpdate project {
      packages = lib.genAttrs systems (system: {
        hall = hall.${system};
        default = hall.${system};
      });
      apps = lib.genAttrs systems (system: {
        hall = app system;
        default = app system;
      });
      overlays.default = final: _prev: {
        hallmark = final.callPackage ./nix/package.nix { };
      };
    };
}
