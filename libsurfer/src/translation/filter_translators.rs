use camino::Utf8Path;
use num::BigUint;
use regex::Regex;
use serde::Deserialize;
use std::fs;
use std::sync::OnceLock;
use surfer_translation_types::{
    BasicTranslator, TranslationPreference, ValueKind, VariableValue, extend_string,
    kind_for_binary_representation,
};
use thiserror::Error;

use crate::{
    translation::check_single_wordlength,
    wave_container::{ScopeId, VarId, VariableMeta},
};

use super::mapping_translators::{parse_color_kind, parse_key_value};

#[derive(Debug, Clone)]
struct FilterMatchValue {
    value: VariableValue,
    value_len: u32,
}

#[derive(Debug, Clone)]
struct NumericRangeSegment {
    start: Option<BigUint>,
    start_inclusive: bool,
    end: Option<BigUint>,
}

impl NumericRangeSegment {
    fn contains(&self, value: &BigUint) -> bool {
        let lower_ok = match &self.start {
            Some(start) if self.start_inclusive => start <= value,
            Some(start) => start < value,
            None => true,
        };
        let upper_ok = match &self.end {
            Some(end) => value <= end,
            None => true,
        };

        lower_ok && upper_ok
    }
}

#[derive(Debug, Clone)]
enum FilterRule {
    Value {
        matcher: FilterMatchValue,
        replacement: String,
        kind: Option<ValueKind>,
    },
    Range {
        ranges: Vec<NumericRangeSegment>,
        replacement: String,
        kind: Option<ValueKind>,
    },
    String {
        matcher: String,
        replacement: String,
        kind: Option<ValueKind>,
    },
    Regex {
        pattern: Regex,
        replacement: String,
        kind: Option<ValueKind>,
    },
}

#[derive(Debug, Clone)]
struct FilterTranslatorConfig {
    name: String,
    bits: Option<u32>,
    variable_name: Option<Regex>,
    variable_type_name: Option<Regex>,
    rules: Vec<FilterRule>,
}

pub struct FilterTranslator {
    config: FilterTranslatorConfig,
}

#[derive(Debug, Error)]
pub enum FilterParseError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error(
        "Rule is missing a match selector; use exactly one of match.value, match.range, match.string, or match.regex"
    )]
    MissingMatchKind,

    #[error(
        "Rule must specify exactly one of match.value, match.range, match.string, or match.regex"
    )]
    MultipleMatchKinds,

    #[error("Rule '{rule_kind}' with value '{match_text}' cannot specify both 'kind' and 'color'")]
    ConflictingStyleFields {
        rule_kind: String,
        match_text: String,
    },

    #[error("Unknown kind/color: {0}")]
    UnknownKindColor(String),

    #[error("Invalid regex '{pattern}': {error}")]
    InvalidRegex {
        pattern: String,
        error: regex::Error,
    },

    #[error("Invalid variable_name regex '{pattern}': {error}")]
    InvalidVariableNameRegex {
        pattern: String,
        error: regex::Error,
    },

    #[error("Invalid variable_type_name regex '{pattern}': {error}")]
    InvalidVariableTypeNameRegex {
        pattern: String,
        error: regex::Error,
    },

    #[error("Invalid numeric value: {0}")]
    InvalidNumericValue(String),

    #[error("Invalid range list: {0}")]
    InvalidRangeList(String),

    #[error("Range start '{start}' is greater than end '{end}'")]
    RangeStartGreaterThanEnd { start: String, end: String },

    #[error("Exact value '{0}' is not valid for a value rule")]
    InvalidExactValue(String),
}

#[derive(Debug, Deserialize)]
struct FilterFileToml {
    name: Option<String>,
    bits: Option<u32>,
    variable_name: Option<String>,
    variable_type_name: Option<String>,
    #[serde(default)]
    rules: Vec<FilterRuleToml>,
}

#[derive(Debug, Deserialize)]
struct FilterRuleToml {
    #[serde(rename = "match")]
    matcher: FilterRuleMatcherToml,
    replace: String,
    kind: Option<String>,
    color: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FilterRuleMatcherToml {
    value: Option<String>,
    range: Option<String>,
    string: Option<String>,
    regex: Option<String>,
}

impl FilterTranslator {
    pub fn new_from_file<P: AsRef<Utf8Path>>(path: P) -> Result<Self, FilterParseError> {
        Ok(Self {
            config: FilterTranslatorConfig::new(path)?,
        })
    }

    pub fn bits(&self) -> Option<u32> {
        self.config.bits
    }
}

impl BasicTranslator<VarId, ScopeId> for FilterTranslator {
    fn name(&self) -> String {
        self.config.name.clone()
    }

    fn basic_translate(&self, num_bits: u32, value: &VariableValue) -> (String, ValueKind) {
        for rule in &self.config.rules {
            match rule {
                FilterRule::Value {
                    matcher,
                    replacement,
                    kind,
                } => {
                    let normalized_value = normalize_value(value, num_bits);
                    if value_matches(matcher, &normalized_value, num_bits) {
                        return (replacement.clone(), kind.unwrap_or(ValueKind::Normal));
                    }
                }
                FilterRule::Range {
                    ranges,
                    replacement,
                    kind,
                } => {
                    if let VariableValue::BigUint(v) = &value
                        && ranges.iter().any(|range| range.contains(v))
                    {
                        let formatted = format_numeric_replacement(replacement, v, num_bits);
                        return (formatted, kind.unwrap_or(ValueKind::Normal));
                    }
                }
                FilterRule::String {
                    matcher,
                    replacement,
                    kind,
                } => {
                    let rendered_value = render_value(num_bits, value);

                    if &rendered_value == matcher {
                        return (replacement.clone(), kind.unwrap_or(ValueKind::Normal));
                    }
                }
                FilterRule::Regex {
                    pattern,
                    replacement,
                    kind,
                } => {
                    let rendered_value = render_value(num_bits, value);
                    if pattern.is_match(&rendered_value) {
                        return (
                            pattern
                                .replace_all(&rendered_value, replacement.as_str())
                                .into_owned(),
                            kind.unwrap_or(ValueKind::Normal),
                        );
                    }
                }
            }
        }

        let rendered_value = render_value(num_bits, value);

        let fallback_kind = kind_for_binary_representation(&rendered_value);
        (rendered_value, fallback_kind)
    }

    fn translates(&self, variable: &VariableMeta) -> eyre::Result<TranslationPreference> {
        if metadata_selector_matches(&self.config, variable) {
            return Ok(TranslationPreference::Prefer);
        }

        if let Some(bits) = self.config.bits {
            return check_single_wordlength(variable.num_bits, bits);
        }

        Ok(TranslationPreference::Yes)
    }
}

#[inline]
fn render_value(num_bits: u32, value: &VariableValue) -> String {
    match value {
        VariableValue::BigUint(v) => {
            format!("{v:0width$b}", width = num_bits as usize)
        }
        VariableValue::String(s) => s.clone(),
    }
}

impl FilterTranslatorConfig {
    fn new<P: AsRef<Utf8Path>>(file: P) -> Result<Self, FilterParseError> {
        parse_file(file)
    }
}

fn parse_file<P: AsRef<Utf8Path>>(path: P) -> Result<FilterTranslatorConfig, FilterParseError> {
    let utf = path.as_ref();
    let content = fs::read_to_string(utf.as_std_path())?;
    let default_name = utf.file_stem().map(std::string::ToString::to_string);
    parse_content_with_default_name(&content, default_name)
}

fn parse_content_with_default_name(
    content: &str,
    default_name: Option<String>,
) -> Result<FilterTranslatorConfig, FilterParseError> {
    let parsed: FilterFileToml = toml::from_str(content)?;
    let name = parsed
        .name
        .or(default_name)
        .unwrap_or_else(|| "Filter translator".to_string());

    let rules = parsed
        .rules
        .into_iter()
        .map(parse_rule)
        .collect::<Result<Vec<_>, _>>()?;

    let variable_name = parsed
        .variable_name
        .map(|pattern| {
            Regex::new(&pattern)
                .map_err(|error| FilterParseError::InvalidVariableNameRegex { pattern, error })
        })
        .transpose()?;

    let variable_type_name = parsed
        .variable_type_name
        .map(|pattern| {
            Regex::new(&pattern)
                .map_err(|error| FilterParseError::InvalidVariableTypeNameRegex { pattern, error })
        })
        .transpose()?;

    Ok(FilterTranslatorConfig {
        name,
        bits: parsed.bits,
        variable_name,
        variable_type_name,
        rules,
    })
}

fn metadata_selector_matches(config: &FilterTranslatorConfig, variable: &VariableMeta) -> bool {
    if config.variable_name.is_none() && config.variable_type_name.is_none() {
        return false;
    }

    let name_match = config
        .variable_name
        .as_ref()
        .is_none_or(|re| re.is_match(&variable.var.name));

    let type_name_match = config.variable_type_name.as_ref().is_none_or(|re| {
        variable
            .variable_type_name
            .as_ref()
            .is_some_and(|type_name| re.is_match(type_name))
    });

    name_match && type_name_match
}

fn parse_rule(rule: FilterRuleToml) -> Result<FilterRule, FilterParseError> {
    let FilterRuleMatcherToml {
        value,
        range,
        string,
        regex,
    } = rule.matcher;

    let populated = [
        value.is_some(),
        range.is_some(),
        string.is_some(),
        regex.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();

    if populated == 0 {
        return Err(FilterParseError::MissingMatchKind);
    }
    if populated > 1 {
        return Err(FilterParseError::MultipleMatchKinds);
    }

    if let Some(match_text) = value {
        let style = parse_style(
            "match.value",
            &match_text,
            rule.kind.as_deref(),
            rule.color.as_deref(),
        )?;
        let (value, value_len) = parse_key_value(&match_text)
            .map_err(|_| FilterParseError::InvalidExactValue(match_text.clone()))?;
        return Ok(FilterRule::Value {
            matcher: FilterMatchValue { value, value_len },
            replacement: rule.replace,
            kind: style,
        });
    }

    if let Some(match_text) = range {
        let style = parse_style(
            "match.range",
            &match_text,
            rule.kind.as_deref(),
            rule.color.as_deref(),
        )?;
        return Ok(FilterRule::Range {
            ranges: parse_range_list(&match_text)?,
            replacement: rule.replace,
            kind: style,
        });
    }

    if let Some(match_text) = string {
        let style = parse_style(
            "match.string",
            &match_text,
            rule.kind.as_deref(),
            rule.color.as_deref(),
        )?;
        return Ok(FilterRule::String {
            matcher: match_text,
            replacement: rule.replace,
            kind: style,
        });
    }

    let Some(pattern_text) = regex else {
        return Err(FilterParseError::MissingMatchKind);
    };
    let style = parse_style(
        "match.regex",
        &pattern_text,
        rule.kind.as_deref(),
        rule.color.as_deref(),
    )?;
    let pattern = Regex::new(&pattern_text).map_err(|error| FilterParseError::InvalidRegex {
        pattern: pattern_text.clone(),
        error,
    })?;
    Ok(FilterRule::Regex {
        pattern,
        replacement: rule.replace,
        kind: style,
    })
}

fn parse_style(
    match_selector: &str,
    match_text: &str,
    kind: Option<&str>,
    color: Option<&str>,
) -> Result<Option<ValueKind>, FilterParseError> {
    match (kind, color) {
        (Some(_), Some(_)) => Err(FilterParseError::ConflictingStyleFields {
            rule_kind: match_selector.to_string(),
            match_text: match_text.to_string(),
        }),
        (Some(kind), None) => parse_color_kind(kind)
            .map(Some)
            .map_err(|_| FilterParseError::UnknownKindColor(kind.to_string())),
        (None, Some(color)) => parse_color_kind(color)
            .map(Some)
            .map_err(|_| FilterParseError::UnknownKindColor(color.to_string())),
        (None, None) => Ok(None),
    }
}

fn parse_range_list(input: &str) -> Result<Vec<NumericRangeSegment>, FilterParseError> {
    let mut ranges = Vec::new();

    for part in input.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        if let Some(bound) = part.strip_prefix(">=") {
            let bound = parse_numeric_value(bound.trim())?;
            ranges.push(NumericRangeSegment {
                start: Some(bound),
                start_inclusive: true,
                end: None,
            });
            continue;
        }

        if let Some(bound) = part.strip_prefix('>') {
            let bound = parse_numeric_value(bound.trim())?;
            ranges.push(NumericRangeSegment {
                start: Some(bound),
                start_inclusive: false,
                end: None,
            });
            continue;
        }

        if let Some((start, end)) = part.split_once('-') {
            let start_value = parse_numeric_value(start.trim())?;
            let end_value = parse_numeric_value(end.trim())?;
            if start_value > end_value {
                return Err(FilterParseError::RangeStartGreaterThanEnd {
                    start: start.trim().to_string(),
                    end: end.trim().to_string(),
                });
            }
            ranges.push(NumericRangeSegment {
                start: Some(start_value),
                start_inclusive: true,
                end: Some(end_value),
            });
        } else {
            let value = parse_numeric_value(part)?;
            ranges.push(NumericRangeSegment {
                start: Some(value.clone()),
                start_inclusive: true,
                end: Some(value),
            });
        }
    }

    if ranges.is_empty() {
        return Err(FilterParseError::InvalidRangeList(input.to_string()));
    }

    Ok(ranges)
}

fn parse_numeric_value(token: &str) -> Result<BigUint, FilterParseError> {
    match parse_key_value(token) {
        Ok((VariableValue::BigUint(value), _)) => Ok(value),
        _ => Err(FilterParseError::InvalidNumericValue(token.to_string())),
    }
}

fn format_numeric_replacement(replacement: &str, value: &BigUint, num_bits: u32) -> String {
    static VALUE_PLACEHOLDER_RE: OnceLock<Regex> = OnceLock::new();
    let re = VALUE_PLACEHOLDER_RE
        .get_or_init(|| Regex::new(r"\{value(?::([^}]+))?\}").expect("valid placeholder regex"));

    re.replace_all(replacement, |captures: &regex::Captures<'_>| {
        let formatter = captures.get(1).map(|m| m.as_str().trim());
        match formatter {
            None | Some("dec") => value.to_string(),
            Some("hex") => format!("0x{value:X}"),
            Some("bin") => format!("0b{value:0width$b}", width = num_bits as usize),
            Some("oct") => format!("0o{value:o}"),
            Some(_) => value.to_string(),
        }
    })
    .into_owned()
}

#[inline]
fn normalize_value(value: &VariableValue, num_bits: u32) -> VariableValue {
    match value {
        VariableValue::BigUint(_) => value.clone(),
        VariableValue::String(s) => VariableValue::String(format!(
            "{extra}{body}",
            extra = extend_string(s, num_bits),
            body = s
        )),
    }
}

fn value_matches(matcher: &FilterMatchValue, value: &VariableValue, num_bits: u32) -> bool {
    match (&matcher.value, value) {
        (VariableValue::BigUint(expected), VariableValue::BigUint(actual)) => expected == actual,
        (VariableValue::String(expected), VariableValue::String(actual)) => {
            let normalized_expected = format!(
                "{extra}{body}",
                extra = extend_string(expected, num_bits.max(matcher.value_len)),
                body = expected
            );
            normalized_expected == *actual
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wave_container::{ScopeId, VarId, VariableMeta, VariableRef};
    use camino::Utf8PathBuf;
    use std::collections::HashMap;
    use surfer_translation_types::BasicTranslator;
    use surfer_translation_types::{ScopeRef, VariableEncoding};

    fn variable_meta(name: &str, type_name: Option<&str>, bits: Option<u32>) -> VariableMeta {
        VariableMeta {
            var: VariableRef {
                path: ScopeRef {
                    strs: vec!["top".to_string()],
                    id: ScopeId::None,
                },
                name: name.to_string(),
                id: VarId::None,
                index: None,
            },
            num_bits: bits,
            variable_type: None,
            variable_type_name: type_name.map(str::to_string),
            index: None,
            direction: None,
            enum_map: HashMap::new(),
            encoding: VariableEncoding::BitVector,
        }
    }

    #[test]
    fn parse_value_range_string_and_regex_rules() {
        let content = r##"
name = "State filter"
bits = 4
variable_name = "^state_"
variable_type_name = "^std_logic_vector"

[[rules]]
match.value = "0x0"
replace = "IDLE"
kind = "normal"

[[rules]]
match.range = "0x1-0x3, 5, 0b1000-0b1001"
replace = "ACTIVE"
color = "#ff8800"

[[rules]]
match.string = "xx01"
replace = "UNKNOWN"
kind = "undef"

[[rules]]
match.regex = "^10(..)$"
replace = "Group:$1"
kind = "warn"
"##;

        let parsed = parse_content_with_default_name(content, None).unwrap();
        assert_eq!(parsed.name, "State filter");
        assert_eq!(parsed.bits, Some(4));
        assert!(parsed.variable_name.is_some());
        assert!(parsed.variable_type_name.is_some());
        assert_eq!(parsed.rules.len(), 4);
    }

    #[test]
    fn translates_prefers_when_variable_name_matches() {
        let content = r#"
variable_name = "^state_"

[[rules]]
match.value = "1"
replace = "ONE"
"#;

        let parsed = parse_content_with_default_name(content, Some("Filter".to_string())).unwrap();
        let translator = FilterTranslator { config: parsed };
        let meta = variable_meta("state_reg", None, Some(8));

        let pref = translator.translates(&meta).unwrap();
        assert_eq!(pref, TranslationPreference::Prefer);
    }

    #[test]
    fn translates_prefers_when_both_name_and_type_match() {
        let content = r#"
variable_name = "^state_"
variable_type_name = "^std_logic_vector"

[[rules]]
match.value = "1"
replace = "ONE"
"#;

        let parsed = parse_content_with_default_name(content, Some("Filter".to_string())).unwrap();
        let translator = FilterTranslator { config: parsed };
        let meta = variable_meta("state_reg", Some("std_logic_vector(7 downto 0)"), Some(8));

        let pref = translator.translates(&meta).unwrap();
        assert_eq!(pref, TranslationPreference::Prefer);
    }

    #[test]
    fn translates_does_not_prefer_if_one_of_both_selectors_fails() {
        let content = r#"
variable_name = "^state_"
variable_type_name = "^std_logic_vector"

[[rules]]
match.value = "1"
replace = "ONE"
"#;

        let parsed = parse_content_with_default_name(content, Some("Filter".to_string())).unwrap();
        let translator = FilterTranslator { config: parsed };
        let meta = variable_meta("state_reg", Some("unsigned(7 downto 0)"), Some(8));

        let pref = translator.translates(&meta).unwrap();
        assert_eq!(pref, TranslationPreference::Yes);
    }

    #[test]
    fn range_rule_supports_mixed_radix_non_consecutive_values() {
        let rules = parse_range_list("0-3, 5, 0x8-0x9").unwrap();
        assert_eq!(rules.len(), 3);
        assert!(rules[0].contains(&BigUint::from(2u32)));
        assert!(rules[1].contains(&BigUint::from(5u32)));
        assert!(rules[2].contains(&BigUint::from(9u32)));
        assert!(!rules.iter().any(|rule| rule.contains(&BigUint::from(4u32))));
    }

    #[test]
    fn range_rule_supports_open_ended_comparisons() {
        let rules = parse_range_list(">=7, >0x10").unwrap();
        assert_eq!(rules.len(), 2);

        assert!(rules[0].contains(&BigUint::from(7u32)));
        assert!(rules[0].contains(&BigUint::from(1_000_000u32)));
        assert!(!rules[0].contains(&BigUint::from(6u32)));

        assert!(rules[1].contains(&BigUint::from(17u32)));
        assert!(!rules[1].contains(&BigUint::from(16u32)));
    }

    #[test]
    fn range_rule_can_include_numeric_value_in_multiple_radices() {
        let content = r#"
[[rules]]
match.range = "8-9"
replace = "dec={value:dec} hex={value:hex} bin={value:bin} oct={value:oct}"
"#;

        let parsed =
            parse_content_with_default_name(content, Some("RangeValueFilter".to_string())).unwrap();
        let translator = FilterTranslator { config: parsed };

        let (text, kind) =
            translator.basic_translate(4, &VariableValue::BigUint(BigUint::from(9u32)));
        assert_eq!(text, "dec=9 hex=0x9 bin=0b1001 oct=0o11");
        assert_eq!(kind, ValueKind::Normal);
    }

    #[test]
    fn translator_applies_first_matching_rule() {
        let content = r#"
name = "State filter"
bits = 4

[[rules]]
match.range = "0-15"
replace = "GENERIC"
kind = "warn"

[[rules]]
match.value = "0x2"
replace = "TWO"
kind = "error"
"#;

        let path = Utf8PathBuf::from("state_filter.toml");
        let parsed =
            parse_content_with_default_name(content, path.file_stem().map(str::to_string)).unwrap();
        let translator = FilterTranslator { config: parsed };

        let (text, kind) =
            translator.basic_translate(4, &VariableValue::BigUint(BigUint::from(2u32)));
        assert_eq!(text, "GENERIC");
        assert_eq!(kind, ValueKind::Warn);
    }

    #[test]
    fn range_rule_falls_back_to_decimal_for_missing_translator_name() {
        let content = r#"
[[rules]]
match.range = "10"
replace = "VALUE={value:DoesNotExist}"
"#;

        let parsed =
            parse_content_with_default_name(content, Some("RangeFilter".to_string())).unwrap();
        let translator = FilterTranslator { config: parsed };

        let (text, kind) =
            translator.basic_translate(8, &VariableValue::BigUint(BigUint::from(10u32)));
        assert_eq!(text, "VALUE=10");
        assert_eq!(kind, ValueKind::Normal);
    }

    #[test]
    fn translator_applies_regex_captures_to_rendered_value() {
        let content = r#"
[[rules]]
match.regex = "^10(..)$"
replace = "tail=$1"
kind = "warn"
"#;

        let parsed =
            parse_content_with_default_name(content, Some("RegexFilter".to_string())).unwrap();
        let translator = FilterTranslator { config: parsed };

        let (text, kind) =
            translator.basic_translate(4, &VariableValue::BigUint(BigUint::from(0b1011u32)));
        assert_eq!(text, "tail=11");
        assert_eq!(kind, ValueKind::Warn);
    }

    #[test]
    fn translator_applies_exact_string_rule() {
        let content = r#"
[[rules]]
match.string = "xx01"
replace = "UNKNOWN"
kind = "undef"
"#;

        let parsed =
            parse_content_with_default_name(content, Some("StringFilter".to_string())).unwrap();
        let translator = FilterTranslator { config: parsed };

        let (text, kind) =
            translator.basic_translate(4, &VariableValue::String("xx01".to_string()));
        assert_eq!(text, "UNKNOWN");
        assert_eq!(kind, ValueKind::Undef);
    }

    #[test]
    fn translator_uses_default_name_from_filename() {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("my_filter.toml")).unwrap();
        fs::write(&path, "[[rules]]\nmatch.value = \"1\"\nreplace = \"ONE\"\n").unwrap();
        let translator = FilterTranslator::new_from_file(&path).unwrap();
        assert_eq!(translator.name(), "my_filter");
    }

    #[test]
    fn translator_requires_only_one_of_kind_or_color() {
        let content = r##"
[[rules]]
match.value = "1"
replace = "ONE"
kind = "warn"
color = "#ff0000"
    "##;

        match parse_content_with_default_name(content, None) {
            Err(FilterParseError::ConflictingStyleFields { .. }) => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn translator_requires_exactly_one_match_selector() {
        let content = r#"
[[rules]]
match.value = "1"
match.regex = "^1$"
replace = "ONE"
"#;

        match parse_content_with_default_name(content, None) {
            Err(FilterParseError::MultipleMatchKinds) => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }
}
