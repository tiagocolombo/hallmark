# The `hall` binary, built from this checkout. Needs only nixpkgs:
#
#   flake.nix        packages.<system>.{hall,default}, with the nixpkgs the
#                    private `rig` input pins
#   nix/default.nix  the same package without flakes or the `rig` input
#
# See docs/decisions/0023-packaging-and-releases.md.
{ lib, rustPlatform }:

let
  cargoToml = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  root = ../.;
in
rustPlatform.buildRustPackage {
  pname = "hallmark";
  inherit (cargoToml.package) version;

  # Everything but build output, local state and files the build never reads,
  # so editing docs or CI doesn't rebuild the binary.
  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.difference root (
      lib.fileset.unions (
        map lib.fileset.maybeMissing [
          ../target
          ../result
          ../.claude
          ../.dev
          ../.direnv
          ../.github
          ../docs
          ../nix
          ../flake.nix
          ../flake.lock
        ]
      )
    );
  };

  cargoLock.lockFile = ../Cargo.lock;

  # The test suite drives `hall` against real git repositories and reads
  # fixtures; CI runs it on every OS. Here we only check the binary starts.
  doCheck = false;
  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    $out/bin/hall --version
    runHook postInstallCheck
  '';

  meta = {
    description = cargoToml.package.description;
    homepage = cargoToml.package.repository;
    license = lib.licenses.mit;
    mainProgram = "hall";
    platforms = lib.platforms.unix;
  };
}
