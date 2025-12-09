# Source Recipe

A source recipe describes how to fetch files (often source code) to be used by other recipes.
In addition to the [common options](./main.md), the options shared by all source types are:

| Field      | Description                                                             | Value                                            |
| ---------- | ----------------------------------------------------------------------- | ------------------------------------------------ |
| type       | The [source type](#source-type).                                        | `"tar.gz"` \| `"tar.xz"` \| `"git"` \| `"local"` |
| url        | Described per [source type](#source-type).                              | String                                           |
| patch      | A path to a patchfile, the path is relative to the root chariot file.   | String                                           |
| regenerate | A script to run _on_ the source, explained further [here](#regenerate). | Code Block                                       |

## Source Type

There are currently three methods supported:

- **"tar.gz"** | **"tar.xz"**

    This method fetches an archive from an URL, checks a checksum against it, extracts it.

    Options:

    | Field | Description                     | Value  |
    | ----- | ------------------------------- | ------ |
    | url   | URL to a tar archive.           | String |
    | b2sum | Blake2 checksum of the archive. | String |

    ````admonish example title="Example Config"
    ```
    source/autoconf {
        type: "tar.gz"
        url: "https://ftp.gnu.org/gnu/autoconf/autoconf-2.72.tar.gz"
        b2sum: "48fff54704176cbf2642230229c628b75c43ef3f810c39eea40cae91dd02e1203d04a544407de96f9172419a94b952865909d969d9e9b6c10879a9d9aeea5ad0"
    }
    ```
    ````

- **"git"**

    This method clones a repo and checks out a specific revision.

    Options:

    | Field    | Description                                                   | Value  |
    | -------- | ------------------------------------------------------------- | ------ |
    | url      | URL to a tar archive.                                         | String |
    | revision | Git revision to check out. Can be a commit hash, tag, branch. | String |

    ````admonish example title="Example Config"
    ```
    source/chariot {
        type: "git"
        url: "https://github.com/elysium-os/chariot"
        revision: "ece5664ddc1c7b0111ae870af0fc2aaa3fdb4c98"
    }
    ```
    ````

- **"local"**

    This method copies a local directory.

    Options:

    | Field | Description                                                             | Value  |
    | ----- | ----------------------------------------------------------------------- | ------ |
    | url   | Path to local directory, the path is relative to the root chariot file. | String |

    ````admonish example title="Example Config"
    ```
    source/support {
        type: "local"
        url: "support"
    }
    ```
    ````

## Regenerate

The regenerate field allows for a script to modify the source before it is used by other recipes.
The regenerate step runs after the patch is applied if one is specified.
Regenerate follows the standard [execution environment](/config/recipe/execution_env.md) with the exception that the current working directory is set to the source root.
