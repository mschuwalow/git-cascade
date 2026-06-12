{
  mkShell,
  cargo,
  cargo-make,
  cargo-release,
  clippy,
  git,
  nixfmt,
  rustPlatform,
  rustc,
  rustfmt,
}:

mkShell {
  packages = [
    cargo
    cargo-make
    cargo-release
    clippy
    git
    nixfmt
    rustc
    rustfmt
  ];

  RUST_SRC_PATH = "${rustPlatform.rustLibSrc}";
}
