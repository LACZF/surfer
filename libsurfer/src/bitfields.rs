use crate::wave_container::VariableMeta;

const EXPANDED_BIT_FIELD_PREFIX: &str = "bit_";

pub fn expanded_bit_field_name(resolved_index: i64) -> String {
    format!("{EXPANDED_BIT_FIELD_PREFIX}{resolved_index}")
}

pub fn bit_resolved_index(bit_idx: u32, meta: Option<&VariableMeta>) -> i64 {
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

pub fn bit_position_from_resolved_index(resolved_index: i64, meta: &VariableMeta) -> Option<u32> {
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

pub fn bit_position_from_field_name(field_name: &str, meta: &VariableMeta) -> Option<u32> {
    field_name
        .strip_prefix(EXPANDED_BIT_FIELD_PREFIX)
        .and_then(|idx| idx.parse::<i64>().ok())
        .and_then(|resolved| bit_position_from_resolved_index(resolved, meta))
}
