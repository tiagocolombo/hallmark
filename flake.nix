{
  description = "Hallmark: the dev shell (declared in dev.toml) and the `hall` package";

  # SSH, not github:owner/repo — the repository is private and Nix's GitHub
  # fetcher calls the API unauthenticated, so it would 404. This uses the
  # SSH key git already has.
  inputs.dev.url = "git+ssh://git@github.com/tiagocolombo/dev";

  # The dev shell, services and apps come from `dev.lib.mkProject` exactly as
  # before. On top of them: the `hall` package (nix/package.nix), built with
  # the nixpkgs `dev` already pins, so there is one nixpkgs in the lock.
  #
  # Evaluating any output still fetches the private `dev` input. Without
  # access to it, build with nix/default.nix, which needs only nixpkgs.
  # See docs/decisions/0023-packaging-and-releases.md.
  outputs =
    { dev, ... }:
    let
      inherit (dev.inputs) nixpkgs;
      inherit (nixpkgs) lib;
      project = dev.lib.mkProject ./dev.toml;
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
