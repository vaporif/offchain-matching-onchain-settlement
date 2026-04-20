{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane = {
      url = "github:ipetkov/crane";
    };
    sp1 = {
      url = "github:vaporif/sp1-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    fenix,
    crane,
    sp1,
    ...
  }: let
    systems = ["x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin"];
    forAllSystems = f:
      nixpkgs.lib.genAttrs systems (system: let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [sp1.overlays.default];
        };
      in
        f {
          inherit pkgs;
          fenixPkgs = fenix.packages.${system};
          craneLib =
            (crane.mkLib pkgs).overrideToolchain
            fenix.packages.${system}.stable.toolchain;
        });
  in {
    packages = forAllSystems ({
      pkgs,
      craneLib,
      ...
    }: let
      src = craneLib.cleanCargoSource ./.;
      commonArgs = {
        inherit src;
        pname = "exchange";
        version = "0.1.0";
        strictDeps = true;
        nativeBuildInputs = [
          pkgs.pkg-config
        ];
        buildInputs =
          [
            pkgs.openssl
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.apple-sdk_15
          ];
      };
      cargoArtifacts = craneLib.buildDepsOnly commonArgs;
    in {
      default = craneLib.buildPackage (commonArgs
        // {
          inherit cargoArtifacts;
          doCheck = false;
        });
    });

    formatter = nixpkgs.lib.genAttrs systems (system: nixpkgs.legacyPackages.${system}.alejandra);

    devShells = nixpkgs.lib.genAttrs systems (system: let
      ctx = forAllSystems (x: x);
      pkgs = ctx.${system}.pkgs;
      fenixPkgs = ctx.${system}.fenixPkgs;

      toolchain = fenixPkgs.combine [
        (fenixPkgs.stable.withComponents [
          "cargo"
          "clippy"
          "llvm-tools-preview"
          "rustc"
          "rustfmt"
          "rust-src"
          "rust-analyzer"
        ])
      ];
    in {
      default = pkgs.mkShell {
        packages =
          [
            toolchain
            pkgs.just
            pkgs.taplo
            pkgs.typos
            pkgs.cargo-nextest
            pkgs.foundry
          ]
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.apple-sdk_15
          ];

        env = {
          RUST_BACKTRACE = "1";
          RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
        };
      };

      sp1 = pkgs.mkShell {
        packages =
          [
            toolchain
            pkgs.just
            pkgs.taplo
            pkgs.typos
            pkgs.cargo-nextest
            pkgs.foundry
          ]
          ++ (with pkgs.sp1."v6.1.0"; [
            cargo-prove
            sp1-rust-toolchain
          ])
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.apple-sdk_15
          ];

        env = {
          RUST_BACKTRACE = "1";
          RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";
        };
      };
    });
  };
}
