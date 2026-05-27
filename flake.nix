{
    inputs = {
        nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
        flake-utils.url = "github:numtide/flake-utils";
    };

    outputs =
        {
            nixpkgs,
            flake-utils,
            ...
        }:
        flake-utils.lib.eachDefaultSystem (
            system:
            let
                pkgs = import nixpkgs { inherit system; };
            in
            {
                devShells.default = pkgs.mkShell {
                    shellHook = "export NIX_SHELL_NAME='chariot'";
                    nativeBuildInputs = with pkgs; [
                        rustup
                        clang
                        lld
                        bun
                        sqlitebrowser
                    ];
                    buildInputs = with pkgs; [
                        pkgconf
                        sqlite
                    ];
                };
            }
        );
}
