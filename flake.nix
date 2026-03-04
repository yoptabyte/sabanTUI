{
  description = "Nix flake for the sabantui project";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;
        cargoLockPath = ./Cargo.lock;
        sabantuiPackage =
          if !builtins.pathExists cargoLockPath then
            throw ''Cargo.lock is required to build sabantui. Run `cargo generate-lockfile` first.''
          else
            pkgs.rustPlatform.buildRustPackage {
              pname = "sabantui";
              version = "0.1.0";
              # Use the working tree so local (uncommitted / untracked) files are included.
              # This avoids flake-source filtering dropping new modules like src/backend/gnome.rs.
              src = lib.cleanSourceWith {
                src = ./.;
                filter = path: type:
                  let base = builtins.baseNameOf path;
                  in !(base == "target" || base == "result" || base == ".direnv");
              };
              cargoLock.lockFile = cargoLockPath;
              nativeBuildInputs = [
                pkgs.pkg-config
                pkgs.makeWrapper
              ];
              buildInputs = [
                pkgs.dbus
                pkgs.glib
                pkgs.wayland
                pkgs.wlr-randr
                pkgs.wl-mirror
                pkgs.wl-gammarelay-rs
                pkgs.gammastep
                pkgs.xorg.libX11
                pkgs.xorg.libXrandr
              ];
              meta = with lib; {
                description = "Universal terminal UI for display management across X11, Wayland, GNOME, and KDE";
                license = licenses.mit;
                mainProgram = "sabantui";
              };
            };

        devShell = pkgs.mkShell {
          name = "sabantui";
          packages = with pkgs; [
            rustc
            cargo
            rustfmt
            clippy
            rust-analyzer
            pkg-config
            dbus
            glib
            wayland
            wlr-randr
            wl-mirror
            wl-gammarelay-rs
            xorg.libX11
            xorg.libXrandr
          ];
          shellHook = ''
            export RUST_BACKTRACE=1
            echo "Launching sabantui development shell for ${system}."

            if command -v wl-gammarelay-rs >/dev/null 2>&1; then
              if ! pgrep -x wl-gammarelay-rs >/dev/null 2>&1; then
                echo "Starting wl-gammarelay-rs in background"
                wl-gammarelay-rs >/dev/null 2>&1 &
              fi
            fi
          '';
        };
      in
      rec {
        packages = {
          sabantui = sabantuiPackage;
          sabanTUI = sabantuiPackage;
          default = sabantuiPackage;
        };

        apps = rec {
          sabantui = {
            type = "app";
            program = "${packages.sabantui}/bin/sabantui";
          };
          sabanTUI = sabantui;
          default = sabantui;
        };

        devShells.default = devShell;

        formatter = pkgs.alejandra;
      }
    );
}
