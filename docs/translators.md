# Available Translators

This page lists the translators currently available in Surfer, grouped by purpose.

## Default

The default translator is **Hexadecimal**.

## Bit/String Display

- **Bit**: Single-bit view (`0`/`1`), preferred for 1-bit signals.
- **Binary**: Plain binary output.
- **Binary (with groups)**: Binary output grouped in nibbles for readability.
- **Octal**: Octal representation.
- **Hexadecimal**: Hex representation (default).
- **ASCII**: Interprets bits as ASCII bytes.
- **String**: For waveform signals with string encoding, preferred when signal encoding is string.
- **Enum**: Uses enum metadata from the waveform to show symbolic names, preferred when enum metadata is present.
- **Event**: Renders VCD event-style variables, preferred for VCD event variables (0-bit event type).

## Numeric Formats

### Integer and Fixed-Point Formats

- **Unsigned**: Unsigned integer interpretation, preferred when variable type name is `unsigned`.
- **Signed**: Two's-complement signed integer interpretation, preferred when variable type name is `signed` or variable type is integer.
- **Unsigned fixed point**: Fixed-point formatting for unsigned fixed-point types, preferred when variable type name is `ufixed`.
- **Signed fixed point**: Fixed-point formatting for signed fixed-point types, preferred when variable type name is `sfixed`.

### Floating-Point Formats

- **FP: 8-bit (E5M2)**: FP8 format with 5 exponent bits and 2 mantissa bits.
- **FP: 8-bit (E4M3)**: FP8 format with 4 exponent bits and 3 mantissa bits.
- **FP: 16-bit IEEE 754**: Half precision (`f16`).
- **FP: bfloat16**: Brain floating-point format.
- **FP: 32-bit IEEE 754**: Single precision (`f32`).
- **FP: 64-bit IEEE 754**: Double precision (`f64`), preferred for real-typed variables.
- **FP: 128-bit IEEE 754**: Optional, available only when Surfer is built with the `f128` feature.

### Posit Formats

- **Posit: 8-bit (no exponent bit)**
- **Posit: 16-bit (one exponent bit)**
- **Posit: 32-bit (two exponent bits)**
- **Posit: quire for 8-bit (no exponent bit)**
- **Posit: quire for 16-bit (one exponent bit)**

## Processor Instructions

- **RV32**: RISC-V 32-bit decoder.
- **RV64**: RISC-V 64-bit decoder.
- **MIPS**: MIPS instruction decoder.
- **LA64**: LoongArch64 instruction decoder.

## Bit-Pattern Statistics

- **Number of ones**: Popcount.
- **Leading ones**: Count of leading `1` bits.
- **Trailing ones**: Count of trailing `1` bits.
- **Leading zeros**: Count of leading `0` bits.
- **Trailing zeros**: Count of trailing `0` bits.
- **Identical MSBs**: Count of identical leading bits (`0` or `1`).

## Color Translators

- **RGB**: Interprets bit fields as RGB and displays a color value.
- **Grayscale**: Interprets value as grayscale intensity.
- **YCbCr**: Interprets value as YCbCr and converts for display.

## Other

- **Clock**: Clock-style rendering, valid for 1-bit signals.
- **LEBxxx**: LEB-style packed value decoding.

## User-Provided Translators

In addition to built-ins, Surfer can load translators at runtime:

- **WASM plugins** (feature `wasm_plugins`): loaded from translator plugin directories.
- **Decoder translators**: loaded from user decoder TOML files in decoder directories.
- **Mapping translators**: loaded from user mapping files in mapping directories.
- **Python translator** (feature `python`): optional Python-based translator support.

For setup details, see the plugin documentation:

- [Translator overview](plugins/README.md)
- [Installing translator plugins](plugins/installing_plugins.md)
- [Writing translator plugins](plugins/writing_plugins.md)
- [Decoders](plugins/decoders.md)
- [Mapping translators](plugins/mapping.md)
