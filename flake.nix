{
  description = "Git-native CLI for cascade rebases across dependent branch stacks";

  inputs = {
    flake-parts.url = "github:hercules-ci/flake-parts";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        flake-parts.flakeModules.easyOverlay
      ];

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      perSystem =
        {
          pkgs,
          final,
          config,
          ...
        }:

        {
          packages = rec {
            default = git-cascade;
            git-cascade = final.callPackage ./nix/git-cascade.nix { };
          };

          overlayAttrs = {
            inherit (config.packages) git-cascade;
          };

          devShells.default = pkgs.callPackage ./nix/dev-shell.nix { };

          formatter = pkgs.nixfmt-tree;
        };
    };
}
