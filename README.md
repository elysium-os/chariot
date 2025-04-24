# Chariot

Chariot is a tool for bootstrapping operating systems. It implements its own DSL for smooth configuration of recipes. It uses the Linux unshare API for building recipes inside of a light reproducible container.

> Much inspiration was taken from [xbstrap](https://github.com/managarm/xbstrap), and in most situations [xbstrap](https://github.com/managarm/xbstrap) is probably the more stable and feature-rich option.

## Usage

The usage is outlined under the chariot `--help` option. Subcommand help is found via `--help <subcommand>`.

## Configuration

See an example at [elysium](https://github.com/elysium-os/elysium) distro repo. For syntax highlighting check out the [tree-sitter grammar](https://github.com/elysium-os/chariot-tree-sitter).
