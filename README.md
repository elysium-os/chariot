# Chariot

Chariot is a tool for bootstrapping operating systems. It implements its own DSL for configuration of recipes. It uses the Linux unshare API for building recipes inside of a light reproducible container.

## Usage

The usage is outlined under the chariot `--help` option. Subcommand help is found via `--help <subcommand>`.

## Documentation

Documentation can be found in the [docs](./docs) directory and can be built with [mdbook](https://github.com/rust-lang/mdBook).
They are built by CI and can be found publicly at <https://elysium-os.github.io/chariot/>.

For syntax highlighting check out the [tree-sitter grammar](https://github.com/elysium-os/chariot-tree-sitter) and [zed-extension](https://github.com/elysium-os/chariot-zed-extension).
