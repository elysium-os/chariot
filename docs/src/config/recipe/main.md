# Recipe

There are four kinds of recipes in Chariot:

- [Source](./source.md)
- [Tool](./common.md#tool-recipe)
- [Package](./common.md#tackage-recipe)
- [Custom](./common.md#tustom-recipe)

Each recipe is made up of a set of options, some of which are shared. Options shared between all recipes are:

| Field        | Description                                                            |
| ------------ | ---------------------------------------------------------------------- |
| options      | The [user defined options](#user-defined-options) used by this recipe. |
| dependencies | List of [dependencies](/config/dependency.md).                         |

## User Defined Options

```admonish warning
User defined options are not to be confused with recipe options.
```

Users can define options using the [option directive](/config/directive.md).
Recipes can then "subscribe" to the relevant options.
A recipe that has "subscribed" to an option will create distinct builds per option set.

````admonish example
For example, take this configuration:
```
@option "buildtype" = [ "debug", "release" ]

package/xyz {
options: ["buildtype"]
...
}
```
The xyz package will have a distinct build for both debug and release build types.
````

The options can then be set on the command line as described in the [CLI ](/cli.md) section.

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

| Modifier | Symbol | Applies To                  | Description                                           |
| -------- | ------ | --------------------------- | ----------------------------------------------------- |
| Runtime  | `*`    | All                         | Dependency is needed at runtime, not just build time. |
| Mutable  | `%`    | [Sources](./source.md) only | Source directory can be modified during build.        |
| Loose    | `!`    | All                         | Dependency will not invalidate its parent.            |

```admonish warning
Usage of the loose and mutable modifiers is discouraged. They are hacky and only meant for use in severe circumstances.
```

````admonish example title="Example Dependencies"
```
%source/libtool
tool/autoconf
image/build-essential
```
````
