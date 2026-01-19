# Overrides

Chariot provides a method to override [source recipes](./config/recipe/source.md) with a local source.
This is done by creating a `.chariot-overrides` file in the same directory as the root chariot config. Each line in this file represents one override in the format of `<source recipe name>: <path to local source>`.

````admonish example title="Example .chariot-overrides"
```
gcc: ../gcc-source
binutils: ../binutils-source
```
````

The goal of overrides is to provide a convenient way to develop/modify sources without having to modify the recipe. It is recommended to gitignore or otherwise make sure the `.chariot-overrides` file is not published to version control.
