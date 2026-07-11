{
  description = "Panda OS development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      # Toolchain (channel, components, targets) comes from rust-toolchain.toml
      # so the flake and rustup users stay in sync automatically.
      rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        packages = [
          rustToolchain
          pkgs.qemu # qemu-system-x86_64 for make run / make test
          pkgs.e2fsprogs # debugfs for building ext2 test images
          pkgs.imagemagick # fuzzy screenshot comparison in graphics tests
          pkgs.python3 # drives the QEMU monitor socket in keyboard tests
          pkgs.gnumake
          pkgs.bash
          pkgs.gnutar
          pkgs.coreutils
        ];
      };
    };
}
