use surfer_translation_types::{TranslatedValue, ValueKind, VariableValue};

use crate::wave_container::VariableMeta;

/// Prefix used to distinguish expanded bit field names from regular subfield names.
const EXPANDED_BIT_FIELD_PREFIX: &str = "__bit_";

/// Generates a field name for an expanded bit at the given resolved index.
///
/// Returns a string like `"__bit_5"` for `resolved_index = 5`.
pub(crate) fn expanded_bit_field_name(resolved_index: i64) -> String {
    format!("{EXPANDED_BIT_FIELD_PREFIX}{resolved_index}")
}

/// Computes the resolved index for a bit, accounting for variable index metadata.
///
/// If the variable has index metadata (e.g., bits [7:0] vs [0:7]), computes the
/// absolute bit index by adding or subtracting `bit_idx` from the LSB. Otherwise,
/// treats `bit_idx` as the resolved index directly.
pub(crate) fn bit_resolved_index(bit_idx: u32, meta: Option<&VariableMeta>) -> i64 {
    meta.and_then(|meta| meta.index)
        .map(|index| {
            if index.msb >= index.lsb {
                index.lsb + i64::from(bit_idx)
            } else {
                index.lsb - i64::from(bit_idx)
            }
        })
        .unwrap_or_else(|| i64::from(bit_idx))
}

/// Reverse lookup: converts a resolved index back to a bit position (0..num_bits).
///
/// Used to verify that a resolved index is valid for the variable and recover
/// the original bit position. Returns `None` if the index is out of bounds.
pub(crate) fn bit_position_from_resolved_index(
    resolved_index: i64,
    meta: &VariableMeta,
) -> Option<u32> {
    let raw = if let Some(index) = meta.index {
        if index.msb >= index.lsb {
            resolved_index.checked_sub(index.lsb)?
        } else {
            index.lsb.checked_sub(resolved_index)?
        }
    } else {
        resolved_index
    };

    let bit_idx = u32::try_from(raw).ok()?;
    if meta.num_bits.is_some_and(|num_bits| bit_idx >= num_bits) {
        None
    } else {
        Some(bit_idx)
    }
}

/// Extracts a bit position from an expanded bit field name.
///
/// Parses the `"__bit_{resolved_index}"` format and converts the resolved index
/// back to a bit position (0..num_bits). Returns `None` if parsing fails or the
/// index is out of bounds.
pub(crate) fn bit_position_from_field_name(field_name: &str, meta: &VariableMeta) -> Option<u32> {
    field_name
        .strip_prefix(EXPANDED_BIT_FIELD_PREFIX)
        .and_then(|idx| idx.parse::<i64>().ok())
        .and_then(|resolved| bit_position_from_resolved_index(resolved, meta))
}

/// Generates a lazy iterator of expanded bit field names and their extracted values.
///
/// For vector-like variables, expands each individual bit into a separate "virtual"
/// subfield with a computed name (using [`expanded_bit_field_name`]) and the
/// corresponding bit's value with appropriate [`ValueKind`].
///
/// # Parameters
///
/// * `bit_count` - Number of bits to expand (typically from `meta.num_bits`)
/// * `meta` - Variable metadata containing index range and bit count
/// * `val` - The variable value, as either [`BigUint`] or a bit string
///
/// # Performance Notes
///
/// - Returns a lazy iterator; expansion happens only when iterating
/// - Precomputes index parameters outside the loop to minimize per-iteration work
/// - Directly matches on byte values for fast character classification
/// - Inlines field name formatting to avoid repeated function calls
pub(crate) fn prepare_expanded_bits<'a>(
    bit_count: u32,
    meta: &'a VariableMeta,
    val: &'a VariableValue,
) -> Box<dyn Iterator<Item = (String, Option<TranslatedValue>)> + 'a> {
    let index_base = meta.index.map(|idx| idx.lsb).unwrap_or(0);
    let should_add = meta.index.map(|idx| idx.msb >= idx.lsb).unwrap_or(true);
    match val {
        VariableValue::BigUint(v) => Box::new((0..bit_count).map(move |bit_idx| {
            let resolved_index = if should_add {
                index_base + i64::from(bit_idx)
            } else {
                index_base - i64::from(bit_idx)
            };
            let bit = if v.bit(bit_idx as u64) { "1" } else { "0" };
            let bit_value = Some(TranslatedValue::new(&bit.to_string(), ValueKind::Normal));
            let field_name = format!("{EXPANDED_BIT_FIELD_PREFIX}{resolved_index}");
            (field_name, bit_value)
        })),
        VariableValue::String(s) => {
            let index_base = meta.index.map(|idx| idx.lsb).unwrap_or(0);
            let should_add = meta.index.map(|idx| idx.msb >= idx.lsb).unwrap_or(true);
            Box::new((0..bit_count).map(move |bit_idx| {
                let resolved_index = if should_add {
                    index_base + i64::from(bit_idx)
                } else {
                    index_base - i64::from(bit_idx)
                };
                let bit_pos = s.len().saturating_sub(1 + bit_idx as usize);
                let bit_value = if bit_pos < s.len() {
                    let bit_byte = s.as_bytes()[bit_pos];
                    let bit_char = bit_byte as char;
                    let kind = match bit_byte {
                        b'0' | b'1' => ValueKind::Normal,
                        b'x' => ValueKind::Undef,
                        b'z' => ValueKind::HighImp,
                        b'-' => ValueKind::DontCare,
                        b'u' | b'w' => ValueKind::Undef,
                        b'h' | b'l' => ValueKind::Weak,
                        _ => ValueKind::Error,
                    };
                    Some(TranslatedValue::new(&bit_char.to_string(), kind))
                } else {
                    None
                };
                let field_name = format!("{EXPANDED_BIT_FIELD_PREFIX}{resolved_index}");
                (field_name, bit_value)
            }))
        }
    }
}

pub(crate) fn extract_bit_value_string(val: &VariableValue, bit_index: u32) -> Option<String> {
    match val {
        VariableValue::BigUint(v) => {
            let bit = if v.bit(bit_index as u64) { "1" } else { "0" };
            Some(bit.to_string())
        }
        VariableValue::String(s) => {
            // String values are MSB-first; bit 0 corresponds to the right-most character.
            let bit_pos = s.len().saturating_sub(1 + bit_index as usize);
            s.as_bytes().get(bit_pos).map(|b| match *b {
                b'0' => "0".to_string(),
                b'1' => "1".to_string(),
                other => char::from(other).to_string(),
            })
        }
    }
}

pub(crate) fn expanded_bit_name(bit_idx: u32, meta: Option<&VariableMeta>) -> String {
    let resolved_index = bit_resolved_index(bit_idx, meta);
    format!("[{resolved_index}]")
}
