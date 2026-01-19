# Recipe

The function of a recipe is to describe how to deterministically fetch or build software. One recipe normally represents one application or library. There are four types of recipes in Chariot:

- [Source](./source.md) — describes how to fetch and prepare, normally software source code, files.
- [Tool](./common.md#tool-recipe) — describes how to build software for use by other recipes.
- [Package](./common.md#tackage-recipe) — describes how to build software for the target operating system being bootstrapped.
- [Custom](./common.md#custom-recipe) — describes how to build software for an unspecified/custom purpose.

Every recipe consists of its type, name, and a set of options. All recipes allow the following options:

| Name         | Description                                                            |
| ------------ | ---------------------------------------------------------------------- |
| options      | The [user defined options](#user-defined-options) used by this recipe. |
| dependencies | List of [dependencies](#dependency).                                   |
|              | See type specific options.                                             |

## User Defined Options

```admonish warning
User defined options are not to be confused with recipe options.
```

Users can define options using the [option directive](/config/directive.md). The options can be set on the command line as described in the [CLI ](/cli.md) section. Recipes can then "subscribe" to relevant options and can use the values passed to influence the recipe.

A recipe that has "subscribed" to an option will create distinct builds per option set. Note that recipes that depend on these recipes will also create distinct builds for those options.

````admonish example
For example, take this configuration:
```
@option "buildtype" = [ "debug", "release" ]

package/xyz {
    options: [ "buildtype" ]
    ...
}
```
The xyz package will have a distinct build for both debug and release build types.
````

Recipes can also choose the allowed values for chosen options.
This functionality is useful when combined with the optional modifier on a [dependency](#dependency).

````admonish example
```
package/xyz {
    options: [ "buildtype" = [ "release" ] ]
    ...
}
```
In this case the xyz package will fail to build for any other values of buildtype than debug.
````

## Dependency

A dependency is made up of modifiers, a namespace, and a name. It takes the form of:

```
<modifier(s)><namespace>/<name>
```

The valid namespaces are:

- `source` refers to a [source recipe](./source.md).
- `tool` refers to a tool recipe.
- `package` refers to a package recipe.
- `custom` refers to a custom recipe.
- `image` refers to an [image package](#image-package).
- `collection` refers to a collection. Collections are defined by the collection [directive](/config/directive.md).

The valid modifiers are:

| Modifier | Symbol | Applies To                 | Description                                                                                                                               |
| -------- | ------ | -------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| Runtime  | `*`    | All                        | Dependency is needed at runtime, not just build time.                                                                                     |
| Optional | `?`    | All                        | Allow the recipe to build even if the dependency cannot be fulfilled. This can happen if the dependency does not support an option value. |
| Mutable  | `%`    | [Source](./source.md) only | Source directory can be modified during build. Note that the modifications will not persist after the build.                              |
| Loose    | `!`    | All                        | Dependency will not invalidate its parent.                                                                                                |

```admonish warning
Usage of the loose discouraged. It is hacky and only meant for use in severe circumstances.
```

````admonish example title="Example Dependencies"
```
%source/libtool
tool/autoconf
image/build-essential
```
````
