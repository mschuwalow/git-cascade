{
  lib,
  rustPlatform,
  installShellFiles,
  git,
}:

let
  cargoToml = fromTOML (builtins.readFile ../crates/git-cascade/Cargo.toml);
in
rustPlatform.buildRustPackage {
  pname = "git-cascade";
  version = cargoToml.package.version;

  src = lib.cleanSource ../.;
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ installShellFiles ];
  nativeCheckInputs = [ git ];

  cargoBuildFlags = [
    "-p"
    "git-cascade"
  ];
  cargoTestFlags = [
    "-p"
    "git-cascade"
    "--features"
    "test-hooks"
  ];

  postInstall = ''
    installShellCompletion --cmd git-cascade \
      --bash <($out/bin/git-cascade completions bash) \
      --fish <($out/bin/git-cascade completions fish) \
      --zsh <($out/bin/git-cascade completions zsh)
  '';

  meta = {
    description = "Git-native CLI for cascade rebases across dependent branch stacks";
    homepage = "https://github.com/mschuwalow/git-cascade";
    license = lib.licenses.mit;
    mainProgram = "git-cascade";
  };
}
