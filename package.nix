{
  lib,
  rustPlatform,
  makeWrapper,
  dust,
  mesa-demos,
  iproute2,
  nethogs,
}:
rustPlatform.buildRustPackage {
  pname = "toptop";
  version = (lib.importTOML ./Cargo.toml).package.version;

  src = lib.cleanSource ./.;

  cargoLock = {
    lockFile = ./Cargo.lock;
  };

  nativeBuildInputs = [
    makeWrapper
  ];

  postInstall = ''
    wrapProgram $out/bin/toptop \
      --prefix PATH : ${lib.makeBinPath [dust mesa-demos iproute2 nethogs]}
  '';

  meta = with lib; {
    description = "modern CLI system monitor";
    homepage = "https://github.com/DazorPlasma/toptop";
    license = licenses.gpl3Plus;
    maintainers = [];
    mainProgram = "toptop";
    platforms = platforms.linux;
  };
}
