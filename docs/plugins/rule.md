# Rule Translators

Rule translators are a more expressive variant of [mapping translators](mapping.md).
They use a TOML rule file and can match values by exact value, numeric ranges, exact strings, or regular expressions.

The configuration files are located either in `.surfer/rules/` in the current working directory or in any of the following, OS-dependent, directories:

| Os      | Path                                                                         |
|---------|------------------------------------------------------------------------------|
| Linux   | `~/.config/surfer/rules/`                                                    |
| Windows | `C:\Users\<Name>\AppData\Roaming\surfer-project\surfer\config\rules\`        |
| macOS   | `/Users/<Name>/Library/Application Support/org.surfer-project.surfer/rules/` |

Rules are evaluated in order, and the first matching rule wins.
If multiple rules match the same value, string, or regex-rendered text, the earlier rule is used and later matching rules are ignored.

The file format is TOML with an optional `name`, an optional `bits`, and one or more `[[rules]]` entries.

Optional metadata selectors:

- `variable_name = "..."`: regex matched against the variable name
- `variable_type_name = "..."`: regex matched against the variable type name from metadata

If `variable_name` is provided and matches, this translator is marked as preferred for that variable.
If `variable_type_name` is provided and matches, this translator is marked as preferred for that variable.
If both are provided, both must match for the translator to be marked as preferred.

`bits` is optional.
If present, the translator only applies to signals with that exact width.
If omitted, it can be selected for any signal with a defined bit width.

Each rule must specify:

- Exactly one of `match.value = "..."`, `match.range = "..."`, `match.string = "..."`, or `match.regex = "..."`
- `replace = "..."`
- Either `kind = "..."` or `color = "#rrggbb"` when explicit styling is wanted

Exact values use the same radices as mapping translators: binary (`0b`), octal (`0o`), decimal, and hex (`0x`).

Range rules always interpret the signal value as an unsigned integer.
They accept comma-separated numeric segments and can mix radices.
Single values, inclusive ranges, and lower-bounded comparisons are all allowed, for example `0-3, 5, >=0x7`.
Range replacements can include the matched numeric value using placeholders:

- `{value}` or `{value:dec}` for unsigned decimal
- `{value:hex}` for hexadecimal
- `{value:bin}` for zero-padded binary using the active signal width
- `{value:oct}` for octal

If a range rule provides `kind` or `color`, that styling is used.

Regex rules match against a text form of the signal value.
For numeric values, the regex sees the zero-padded binary representation for the active signal width.
For string values, the regex sees the raw string value. This includes "binary" variables that contain `x`, `-` etc.
Regex replacements support capture groups such as `$1` and `$2`.

Example:

```toml
name = "State rule"
bits = 4
variable_name = "^state_"
variable_type_name = "^std_logic_vector"

[[rules]]
match.value = "0x0"
replace = "IDLE"
kind = "normal"

[[rules]]
match.range = "0x1-0x3, 5, 0b1000-0b1001"
replace = "ACTIVE ({value:hex})"
color = "#ff8800"

[[rules]]
match.string = "xx01"
replace = "UNKNOWN"
kind = "undef"

[[rules]]
match.regex = "^10(..)$"
replace = "tail=$1"
kind = "warn"
```

This example does the following:

- Maps exact value `0` to `IDLE`
- Maps `1`, `2`, `3`, `5`, `8`, and `9` to `ACTIVE (1)` through `ACTIVE (9)` using the hex for the matched value
- Maps exact string `xx01` to `UNKNOWN`
- Maps other four-bit numeric values whose zero-padded binary representation starts with `10` to `tail=<last two bits>`
- The regex rule also matches `8` (`1000`) and `9` (`1001`), but it does not apply to them here because the earlier range rule already matched and first matching rule wins
