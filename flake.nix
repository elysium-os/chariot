{
    inputs = {
        nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";
        flake-utils.url = "github:numtide/flake-utils";
    };

    outputs = { self, nixpkgs, flake-utils, ... }: flake-utils.lib.eachDefaultSystem (system:
        let pkgs = import nixpkgs { inherit system; }; in {
            devShells.default = pkgs.mkShell {
                shellHook = "export NIX_SHELL_NAME='chariot'";
                buildInputs = with pkgs; [ cargo ];
                nativeBuildInputs = with pkgs; [ wget libarchive ];
            };

            defaultPackage = pkgs.rustPlatform.buildRustPackage {
                name = "chariot";
                src = self;

                cargoLock.lockFile = ./Cargo.lock;

                nativeBuildInputs = with pkgs; [ installShellFiles ];

                postInstall = ''
                    installShellCompletion --name chariot.bash --bash <($out/bin/chariot completions bash)
                    installShellCompletion --name chariot.fish --fish <($out/bin/chariot completions fish)
                    installShellCompletion --name __chariot --zsh <($out/bin/chariot completions zsh)
                '';

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
