# Tool / Package / Custom Recipe

The tool, package, and custom recipes all describe how to build software.
Most of the functionality (and thus options) are shared between these recipe types, here are the options they share:

| Field        | Description                            | Value     |
| ------------ | -------------------------------------- | --------- |
| configure    | A script to configure the recipe.      | CodeBlock |
| build        | A script to build the recipe.          | CodeBlock |
| install      | A script to install the recipe         | CodeBlock |
| always_clean | Whether to always wipe the build cache | Boolean   |

### Execution Environment

The execution environment for configure/build/install follows the the standard [execution environment](/config/recipe/execution_env.md).
In addition to the standard environment these codeblocks define the following environment variables:

- `PARALLELISM` is set to the number of worker threads expected.
- `PREFIX` is the installation prefix to use. For packages and custom recipes this is the configured prefix. For tools it is an internal one.
- `BUILD_DIR` is set to the path of the build directory.
- `INSTALL_DIR` is set to the path of the installation directory.

## Tool Recipe

The tool recipe is for building tools (such as cross compiler etc) for the host (the chariot container).
These tools can then be used as dependencies for other packages.

## Package Recipe

The package recipe is for building packages for the target platform.

## Custom Recipe

This recipe does not have a specific purpose it is there to fill needs that both package and tool recipes fail to.
