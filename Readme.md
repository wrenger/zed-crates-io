# Crates.io LSP Server

Fetches information about outdated dependencies for Cargo.toml files.

## Work In Progress

This extension is currently in development and may not work as expected.
Automatic installation of the language server is not yet supported.

You have to manually install the [language server](crates-io-lsp/).

And then add the following Zed configuration:

```json
{
  "lsp": {
    "crates-io": {
      "initialization_options": {},
      "binary": {
        "path": "<path/to>/zed-crates-io/target/debug/crates-io-lsp",
        "args": []
      }
    }
  }
}
```
