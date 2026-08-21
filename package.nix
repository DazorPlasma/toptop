{
  lib,
  rustPlatform,
}:
rustPlatform.buildRustPackage {
  pname = "toptop";
  version = (lib.importTOML ./Cargo.toml).package.version;

  src = lib.cleanSource ./.;

  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  meta = with lib; {
    description = "modern CLI system monitor";
    homepage = "https://github.com/DazorPlasma/toptop";
    license = licenses.gpl3Plus;
    maintainers = [];
    mainProgram = "toptop";
    platforms = platforms.linux;
  };
}
