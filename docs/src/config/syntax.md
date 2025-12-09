# Syntax

### Whitespace

Whitespace separates other tokens but has no meaning beyond that.

### Comment

There are two types of comments in chariot:

- Single-line comments (starting with `//`).

````admonish example
```
// This is a single-line comment
```
````

- Multi-line comments (starting with `/*` and ending with `*/`).

````admonish example
```
/*
    This is a multi-line comment
*/
```
````

### Identifier

- Starts with a letter.
- Continues with letter, number, `_`, `.`, `-`, `+`

### String

- Wrapped in double quotes.
- Any characters are allowed except newline.

### Code Block

- Wrapped with language tags `<lang></lang>`.
    - Currently valid languages are:
        - `sh`, `shell`, `bash`
        - `py`, `python`
- Note that if a closing tag (`</lang>`) is present but the language does not match, the tag will be interpreted as code.

````admonish example
```
<sh>
    echo "Hello world!"
</sh>
```
````

### Directive

- Starts with `@`.
- Continues with letters, numbers, `_`, `-`.

````admonish example
```
@env
@option
@n0ne_3xistent-directive
```
````

# Structure

There are two top level constructs:

## Directives

The syntax for directives differs widely depending on the directive.
The list of directives is available on [this](directive.md) page along with examples for them.

## Recipe Definition

Recipes definitions are structured as follows:

```
<recipe type>/<name> {
    <option key>: <option value>
}
```

Recipes options are explained in more detail [here](./recipe/main.md).
