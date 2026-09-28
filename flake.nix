{
  description = "Vobos";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    flake-utils.url = "github:numtide/flake-utils";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    naersk = {
      url = "github:nix-community/naersk";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      fenix,
      naersk,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
        };

        toolchain = fenix.packages.${system}.latest.toolchain;

        naersk-lib = pkgs.callPackage naersk {
          cargo = toolchain;
          rustc = toolchain;
        };

        nativeBuildInputs = with pkgs; [
          pkg-config
          cmake
          ninja

          shaderc
          spirv-headers
          vulkan-headers

          llvmPackages.clang
          llvmPackages.libclang
        ];

        buildInputs = with pkgs; [
          alsa-lib
          libxkbcommon
          wayland
          libxcb
          pipewire
          libglvnd
          libgbm
          stdenv.cc.cc.lib
          vulkan-loader
        ];
      in
      {
        packages.default = naersk-lib.buildPackage {
          pname = "vobos";
          version = "0.1.0";

          src = ./.;

          nativeBuildInputs = nativeBuildInputs ++ [ pkgs.autoPatchelfHook ];

          inherit buildInputs;

          # Winit loads these libraries dynamically on Wayland.
          runtimeDependencies = with pkgs; [ wayland libxkbcommon ];

          postInstall = ''
            install -Dm644 assets/com.seedse.vobos.desktop \
              "$out/share/applications/com.seedse.vobos.desktop"
            install -Dm644 assets/com.seedse.vobos.svg \
              "$out/share/icons/hicolor/scalable/apps/com.seedse.vobos.svg"
          '';

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
        };

        devShells.default = pkgs.mkShell {
          inherit nativeBuildInputs buildInputs;

          packages = with pkgs; [
            toolchain
            rust-analyzer
            git
            just
            bacon
            vulkan-tools
          ];

          RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust/library";

          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath buildInputs;

          shellHook = ''
            unset SOURCE_DATE_EPOCH
          '';
        };
      }
    );
}
