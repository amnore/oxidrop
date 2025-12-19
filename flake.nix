{
  outputs = { self, nixpkgs }:
  let
    lib = nixpkgs.lib;
    forAllSystems = lib.genAttrs lib.systems.flakeExposed;
  in
  {
    packages = forAllSystems (sys: {
      default = nixpkgs.legacyPackages.${sys}.callPackage
        ({ rustPlatform }: rustPlatform.buildRustPackage {
          name = "oxidrop";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
        }) {};
    });
  };
}
