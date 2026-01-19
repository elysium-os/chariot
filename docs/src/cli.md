# CLI Reference

This page captures the currently implemented CLI surface (`src/main.rs`).

## Global
- `--config <path>`: Path to config file (default `config.chariot`).
- `--cache <path>`: Path to cache directory (default `.chariot-cache`).
- `--rootfs-version <tag>`: Override rootfs version tag (default baked into release).
- `--no-lockfile`: Skip acquiring the cache lockfile (use with care).
- `-v, --verbose`: Stream logs while building.
- `-o, --option key=value`: Provide option values; can also be set via environment `OPTION_<NAME>=value`.

## Subcommands

### build
`chariot build [OPTIONS] <recipe>...`
- `--prefix <path>`: Install prefix for package/custom recipes (`/usr` by default).
- `-j, --parallelism <n>`: Parallelism for build scripts (defaults to host CPUs).
- `-w, --clean`: Force a clean build directory for the targeted recipes.
- `--ignore-changes`: Do not rebuild dependencies even if they changed.

### exec
`chariot exec [OPTIONS] [--] <command...>`
- `--recipe-context <ns/name>`: Run inside a recipe context with its dependencies mounted.
- `-p, --package <pkg>`: Extra packages to install into the container.
- `-d, --dependency <ns/name>`: Extra recipe dependencies to mount/install.
- `-e, --env KEY=VAL`: Extra environment variables.
- `-m, --mount from=to[:ro]`: Bind mount host paths (`:ro` for read-only).
- `--uid <id>`, `--gid <id>`: Override user/group (default 1000/1000).
- `--rw`: Make the container writable (read-only by default).
- `--cwd <path>`: Set working directory inside the container.

### purge
Remove recipes from cache that are no longer in the config.

### list
List cached recipes with status, size, and last build timestamp.

### wipe
`chariot wipe <cache|rootfs|proc-cache|recipe [--all] [<recipe>...]>`  
Delete parts of the cache/rootfs. `recipe` accepts specific recipes or `--all`.

### path
`chariot path <ns/name> [--raw]`  
Print the install/output path for a recipe (raw path only with `--raw`).

### hash
`chariot hash <ns/name> [--raw]`  
Print the recipe hash (machine-readable with `--raw`).

### logs
`chariot logs <ns/name> [kind]`  
Print stage logs for a recipe (defaults to `build.log`).

### completions
`chariot completions <shell>`  
Generate shell completion scripts.
