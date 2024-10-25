# Chariot
Chariot is a tool for bootstrapping operating systems.  
  
Much inspiration was taken from [xbstrap](https://github.com/managarm/xbstrap), and in most situations [xbstrap](https://github.com/managarm/xbstrap) is probably the more stable and feature-rich option.

## Usage
`chariot [options] [targets]`

## Options
`--config=<file>` override the default config file path  
`--exec=<command>` execute a command in the container  
`--wipe-container` reset the container  
`--verbose` turn on verbose logging (stdout=on)  
`--hide-conflicts` hide conflicts during dependency directory population (recommended for automated scripts)  

## Config
Charon uses a DSL (Domain Specific Language) since version 2.


### Source Recipe
```
source/<identifier> {
    url: <url>
    type: <source_type>
    b2sum: <hash>
    patch: <patch>
    dependencies: [ source/<identifier> host/<identifier> target/<identifier> ]
    strap: {
        <strap_block>
    }
}
```

- `identifer`: a unique identifier (within the namespace) which follows the following rules:
    - first character is `a-z`, `A-Z`, or `_`
    - the rest consists of `a-z`, `A-Z`, `0-9`, `_`, or `-`
- `url`: the local or remote url of the source
- `source_type`:
    - `tar.gz`
    - `tar.xz`
    - `local`
- `hash`: a blake2b sum required for archives
    - TODO: explain how to generate hashes for chariot
- `patch`: name of a patch in the patch directory which will be applied to the source
    - TODO: explain how to generate patches for chariot
- `strap_block`: shell code for bootstrapping source. it is ran after the patch
    - TODO: describe what embed are available

### Host/Target Recipes
```
host/<identifer> {
    source: <source>
    dependencies [ source/<identifier> host/<identifier> target/<identifier> ]
    configure {
        <configure_block>
    }
    build {
        <build_block>
    }
    install {
        <install_block>
    }
}

target/<identifer> {
    ...
}
```
- `identifier`: refer to source recipe
- `source`: the source to use, does not require namespace prefix
- `configure_block`: shell code for configuring the recipe
    - TODO: describe what embed are available
- `build_block`: shell code for building the recipe
    - TODO: describe what embed are available
- `install_block`: shell code for installing the recipe
    - TODO: describe what embed are available