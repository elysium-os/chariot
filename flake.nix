{
    inputs = {
        nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";
        flake-utils.url = "github:numtide/flake-utils";
    };

    outputs = { self, nixpkgs, flake-utils, ... }: flake-utils.lib.eachDefaultSystem (system:
        let pkgs = import nixpkgs { inherit system; }; in {
            devShells.default = pkgs.mkShell {
                shellHook = "export NIX_SHELL_NAME='chariot'";
                nativeBuildInputs = with pkgs; [ wget libarchive ];
            };

            defaultPackage = pkgs.rustPlatform.buildRustPackage {
                name = "chariot";
                src = self;

                cargoLock.lockFile = ./Cargo.lock;

                meta = {
                    description = "A tool for building and bootstrapping operating systems.";
                    homepage = "https://github.com/elysium-os/chariot";
                    license = pkgs.lib.licenses.bsd3;
                    maintainers = with pkgs.lib.maintainers; [ wux ];
                };
            };
        }
    );
}
