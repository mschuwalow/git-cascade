{
  description = "Git-native CLI for cascade rebases across dependent branch stacks";

  inputs = {
    flake-parts.url = "github:hercules-ci/flake-parts";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      perSystem =
        { pkgs, ... }:
        let
          git-cascade = pkgs.callPackage ./nix/git-cascade.nix { };
        in
        {
          packages = {
            default = git-cascade;
            inherit git-cascade;
          };

          checks.default = git-cascade;

          devShells.default = pkgs.callPackage ./nix/dev-shell.nix { };

          formatter = pkgs.nixfmt-tree;
        };
    };
}
