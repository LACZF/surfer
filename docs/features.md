# Compile Features

There are a number of compile time features that can be enabled or disabled.

| Feature | Description | Default |
| ------- | ----------- | ------- |
| `accesskit` |  Accessibility support. | No`*`|
| `https` | Download waveform files using https. | Yes |
| `performance_plot` | The `show_performance` command and the drawing performance plot window. | Yes |
| `python` | Python translators. | No |
| `wasm_plugins` | WASM translator plugins. | Yes |

`*` Included in pre-built binaries.

The 128-bit floating-point translator is available when compiling with a nightly Rust compiler.
