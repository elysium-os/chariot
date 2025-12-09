# Execution Environment

Chariot executes all scripts in its container. This means the execution environment is well defined.
The root filesystem is a debian one. This rootfs is built using CI in the [charon-rootfs](https://github.com/elysium-os/chariot-rootfs) repository.

### Current Working Directory

The current working directory is set to the build directory.

### Environment Variables

Chariot exposes several environment variables to all execution environments:

- `SOURCES_DIR` is set to the path of a directory where source recipes are mounted. For example the shellscript `$SOURCE_DIR/xyz` would refer to a dependency `source/xyz`.
- `CUSTOM_DIR` is set to the path of a directory where custom recipes are mounted. For example the shellscript `$CUSTOM_DIR/xyz` would refer to a dependency `custom/xyz`.
- `SYSROOT_DIR` is set to the path of the sysroot. The sysroot is where all package recipes are installed. Unlike sources and customs these are not installed separately but installed together.
- User defined options create environment variables in the format of `OPTION_<option>`. For example if there was a user defined option called `buildtype` an environment variable called `OPTION_buildtype` would be created. The value of these variables are naturally set to the value of the options.
- All environment variables defined using the [env directive](/config/directive.md).

### Image Package

Image package refers to normal debian packages as chariot uses a debian rootfs.
The image packages present are chosen by the recipe dependencies as well as the global package set.

Note that the global package set includes both some hardcoded packages described [here](https://github.com/elysium-os/chariot/blob/main/src/rootfs.rs#L18) and packages defined using the [global_pkg directive](/config/directive.md).

Debian package search can be useful to find packages: <https://packages.debian.org/index>.
