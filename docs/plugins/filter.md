# Filter Translators

Filter translators are a more expressive variant of [mapping translators](mapping.md).
They use a TOML rule file and can match values by exact value, numeric ranges, exact strings, or regular expressions.

The configuration files are located either in `.surfer/filters/` in the current working directory or in any of the following, OS-dependent, directories:

| Os      | Path                                                                           |
|---------|--------------------------------------------------------------------------------|
| Linux   | `~/.config/surfer/filters/`                                                    |
| Windows | `C:\Users\<Name>\AppData\Roaming\surfer-project\surfer\config\filters\`        |
| macOS   | `/Users/<Name>/Library/Application Support/org.surfer-project.surfer/filters/` |

Rules are evaluated in order, and the first matching rule wins.
If multiple rules could match the same value, string, or regex-rendered text, the earlier rule is used and later matching rules are ignored.

The file format is TOML with an optional `name`, an optional `bits`, and one or more `[[rules]]` entries.

`bits` is optional.
If present, the translator only applies to signals with that exact width.
If omitted, it can be selected for any signal with a defined bit width.

Each rule must specify:

- `rule = "value"`, `"range"`, `"string"`, or `"regex"`
- `replace = "..."`
- Either `kind = "..."` or `color = "#rrggbb"` when explicit styling is wanted

Exact values use the same radices as mapping translators: binary (`0b`), octal (`0o`), decimal, and hex (`0x`).

Range rules accept comma-separated numeric segments and can mix radices.
Single values and inclusive ranges are both allowed, for example `0-3, 5, 0x8-0x9`.

Regex rules operate on the rendered raw value.
For numeric values, that is the zero-padded binary representation for the active signal width.
For string values, that is the raw string value.
Regex replacements support capture groups such as `$1` and `$2`.

Example:

```toml
name = "State filter"
bits = 4

[[rules]]
rule = "value"
match = "0x0"
replace = "IDLE"
kind = "normal"

[[rules]]
rule = "range"
match = "0x1-0x3, 5, 0b1000-0b1001"
replace = "ACTIVE"
color = "#ff8800"

[[rules]]
rule = "string"
match = "xx01"
replace = "UNKNOWN"
kind = "undef"

[[rules]]
rule = "regex"
pattern = "^10(..)$"
replace = "tail=$1"
kind = "warn"
```

This example does the following:

- Maps exact value `0` to `IDLE`
- Maps `1`, `2`, `3`, `5`, `8`, and `9` to `ACTIVE`
- Maps exact string `xx01` to `UNKNOWN`
- Maps any four-bit rendered value starting with `10` to `tail=<last two bits>`
