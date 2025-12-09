# Directive

Directives are global configuration statements prefixed with `@`. They are processed before recipe definitions.

| Directive  | Description                                                         | Value                                                                 | Example                                                   |
| ---------- | ------------------------------------------------------------------- | --------------------------------------------------------------------- | --------------------------------------------------------- |
| import     | Import another chariot file.                                        | Path to a chariot file, relative to the current file. Supports globs. | `@import "recipes/*.chariot"`                             |
| env        | Declare a global environment variable.                              | Key-value pair of environment variable name and value.                | `@env "CLICOLOR_FORCE" = "1"`                             |
| collection | Create a collection of [dependencies](./recipe/main.md#dependency). | Key-value pair of collection name and its dependencies.               | `@collection autotools = [ tool/autoconf tool/automake ]` |
| option     | Declare an option.                                                  | Key-value pair of option name and valid values.                       | `@option "buildtype" = [ "debug", "release" ]`            |
| global_pkg | Add global image packages                                           | Either a package or a list of packages.                               | `@option global_pkg build-essentials`                     |
