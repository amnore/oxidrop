{
  outputs = { self, nixpkgs }:
  let
    lib = nixpkgs.lib;
    forAllSystems = lib.genAttrs lib.systems.flakeExposed;
  in
  {
    packages = forAllSystems (sys: {
      default = nixpkgs.legacyPackages.${sys}.callPackage
        ({ rustPlatform, dbus, pkg-config, protobuf }: rustPlatform.buildRustPackage {
          name = "oxidrop";
          src = ./.;

          cargoLock.lockFile = ./Cargo.lock;
          cargoLock.outputHashes = {
            "mdns-sd-0.10.4" = "sha256-y8pHtG7JCJvmWCDlWuJWJDbCGOheD4PN+WmOxnakbE4=";
            "rqs_lib-0.11.5" = "sha256-5nd4ISIavd1GptjhuPoG+zN3iHTTmlc+uO695RnvkCs=";
            "sys_metrics-0.2.7" = "sha256-hCialGTBTA7QdiqlTV6sBdiXpgDEB+96IWLFV2M+36o=";
          };

          buildInputs = [
            dbus
          ];
          nativeBuildInputs = [
            pkg-config
            protobuf
          ];
        }) {};
    });

    devShell = forAllSystems (sys:
    let
      pkgs = nixpkgs.legacyPackages.${sys};
    in
    pkgs.mkShell {
      inputsFrom = [ self.packages.${sys}.default ];
      packages = with pkgs; [ rust-analyzer rustfmt ];
    });
  };
}
