# Crates.io LSP Server

Fetches information about outdated dependencies for `Cargo.toml` files.

> [!WARNING]
> This extension is currently in development and may not work as expected.


## Manual Installation

Download the repo:

```sh
git@github.com:wrenger/zed-crates-io.git
```

Install it with the `zed: install dev extension` command in zed.
Or in the Extension tab.

## Language Server Installation

The extension **automatically** installs the [language server](crates-io-lsp/) for Linux/x86_64, MacOs/x86_64, and MacOs/aarch64.

All other platforms have to build it manually.
And then add the following Zed configuration:

```json
{
  "lsp": {
    "crates-io": {
      "initialization_options": {},
      "binary": {
        "path": "<path/to>/zed-crates-io/target/release/crates-io-lsp",
        "args": []
      }
    }
  }
}
```

Possible arguments are:
- `--endpoint`: The endpoint to the language server. Default is `https://index.crates.io`
- `--token`: Optional token for the API endpoint.
