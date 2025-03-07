{
    inputs = {
        nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.05";
        flake-utils.url = "github:numtide/flake-utils";
    };

    outputs = { self, nixpkgs, flake-utils, ... } @ inputs: flake-utils.lib.eachDefaultSystem (system:
        let
            pkgs = import nixpkgs { inherit system; };
            inherit (pkgs) lib stdenv mkShell fetchFromGitLab buildGoModule;
        in {
            devShells.default = mkShell {
                shellHook = "export DEVSHELL_PS1_PREFIX='chariot'";
                nativeBuildInputs = with pkgs; [
                    wget
                    gnumake
                    gcc14
                    gdb
                    nodejs
                ];
            };

            defaultPackage = stdenv.mkDerivation rec {
                name = "chariot";
                src = self;

                nativeBuildInputs = with pkgs; [ gcc14 ];

                installPhase = ''
                    mkdir -p $out/bin
                    cp chariot $out/bin/
                '';

                meta = {
                    description = "A tool for building and bootstrapping operating systems.";
                    homepage = https://git.thenest.dev/wux/chariot;
                    maintainers = with lib.maintainers; [ wux ];
                };
            };
        }
    );
}
