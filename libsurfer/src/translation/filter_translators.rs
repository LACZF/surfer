use camino::Utf8Path;
use num::BigUint;
use regex::Regex;
use serde::Deserialize;
use std::fs;
use surfer_translation_types::{
    BasicTranslator, TranslationPreference, ValueKind, VariableValue, extend_string,
    kind_for_binary_representation,
};
use thiserror::Error;

use crate::{
    translation::check_single_wordlength,
    wave_container::{ScopeId, VarId, VariableMeta},
};

use super::mapping_translators::{key_display, parse_color_kind, parse_key_value};

#[derive(Debug, Clone)]
struct FilterMatchValue {
    value: VariableValue,
    value_len: u32,
}

#[derive(Debug, Clone)]
struct NumericRangeSegment {
    start: BigUint,
    end: BigUint,
}

impl NumericRangeSegment {
    fn contains(&self, value: &BigUint) -> bool {
        &self.start <= value && value <= &self.end
    }
}

#[derive(Debug, Clone)]
enum FilterRule {
    Value {
        matcher: FilterMatchValue,
        replacement: String,
        kind: ValueKind,
    },
    Range {
        ranges: Vec<NumericRangeSegment>,
        replacement: String,
        kind: ValueKind,
    },
    String {
        matcher: String,
        replacement: String,
        kind: ValueKind,
    },
    Regex {
        pattern: Regex,
        replacement: String,
        kind: ValueKind,
    },
}

#[derive(Debug, Clone)]
struct FilterTranslatorConfig {
    name: String,
    bits: Option<u32>,
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

    #[error("Unknown rule kind: {0}")]
    UnknownRuleKind(String),

    #[error("Rule '{rule_kind}' is missing required field '{field}'")]
    MissingField {
        rule_kind: String,
        field: &'static str,
    },

    #[error("Rule '{rule_kind}' cannot specify both 'kind' and 'color'")]
    ConflictingStyleFields { rule_kind: String },

    #[error("Unknown kind/color: {0}")]
    UnknownKindColor(String),

    #[error("Invalid regex '{pattern}': {error}")]
    InvalidRegex {
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
    #[serde(default)]
    rules: Vec<FilterRuleToml>,
}

#[derive(Debug, Deserialize)]
struct FilterRuleToml {
    rule: String,
    #[serde(rename = "match")]
    match_text: Option<String>,
    pattern: Option<String>,
    replace: String,
    kind: Option<String>,
    color: Option<String>,
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
        let normalized_value = normalize_runtime_value(value, num_bits);
        let rendered_value = render_runtime_value(&normalized_value, num_bits);

        for rule in &self.config.rules {
            match rule {
                FilterRule::Value {
                    matcher,
                    replacement,
                    kind,
                } => {
                    if value_matches(matcher, &normalized_value, num_bits) {
                        return (replacement.clone(), *kind);
                    }
                }
                FilterRule::Range {
                    ranges,
                    replacement,
                    kind,
                } => {
                    if let VariableValue::BigUint(v) = &normalized_value
                        && ranges.iter().any(|range| range.contains(v))
                    {
                        return (replacement.clone(), *kind);
                    }
                }
                FilterRule::String {
                    matcher,
                    replacement,
                    kind,
                } => {
                    if &rendered_value == matcher {
                        return (replacement.clone(), *kind);
                    }
                }
                FilterRule::Regex {
                    pattern,
                    replacement,
                    kind,
                } => {
                    if pattern.is_match(&rendered_value) {
                        return (
                            pattern
                                .replace_all(&rendered_value, replacement.as_str())
                                .into_owned(),
                            *kind,
                        );
                    }
                }
            }
        }

        let fallback_kind = kind_for_binary_representation(&rendered_value);
        (rendered_value, fallback_kind)
    }

    fn translates(&self, variable: &VariableMeta) -> eyre::Result<TranslationPreference> {
        if let Some(bits) = self.config.bits {
            return check_single_wordlength(variable.num_bits, bits);
        }

        Ok(variable
            .num_bits
            .is_some()
            .then_some(TranslationPreference::Yes)
            .unwrap_or(TranslationPreference::No))
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

    Ok(FilterTranslatorConfig {
        name,
        bits: parsed.bits,
        rules,
    })
}

fn parse_rule(rule: FilterRuleToml) -> Result<FilterRule, FilterParseError> {
    let style = parse_style(&rule.rule, rule.kind.as_deref(), rule.color.as_deref())?;
    match rule.rule.to_ascii_lowercase().as_str() {
        "value" => {
            let match_text = rule
                .match_text
                .ok_or_else(|| FilterParseError::MissingField {
                    rule_kind: rule.rule.clone(),
                    field: "match",
                })?;
            let (value, value_len) = parse_key_value(&match_text)
                .map_err(|_| FilterParseError::InvalidExactValue(match_text.clone()))?;
            Ok(FilterRule::Value {
                matcher: FilterMatchValue { value, value_len },
                replacement: rule.replace,
                kind: style,
            })
        }
        "range" => {
            let match_text = rule
                .match_text
                .ok_or_else(|| FilterParseError::MissingField {
                    rule_kind: rule.rule.clone(),
                    field: "match",
                })?;
            Ok(FilterRule::Range {
                ranges: parse_range_list(&match_text)?,
                replacement: rule.replace,
                kind: style,
            })
        }
        "string" => {
            let match_text = rule
                .match_text
                .ok_or_else(|| FilterParseError::MissingField {
                    rule_kind: rule.rule.clone(),
                    field: "match",
                })?;
            Ok(FilterRule::String {
                matcher: match_text,
                replacement: rule.replace,
                kind: style,
            })
        }
        "regex" => {
            let pattern_text = rule.pattern.ok_or_else(|| FilterParseError::MissingField {
                rule_kind: rule.rule.clone(),
                field: "pattern",
            })?;
            let pattern =
                Regex::new(&pattern_text).map_err(|error| FilterParseError::InvalidRegex {
                    pattern: pattern_text.clone(),
                    error,
                })?;
            Ok(FilterRule::Regex {
                pattern,
                replacement: rule.replace,
                kind: style,
            })
        }
        other => Err(FilterParseError::UnknownRuleKind(other.to_string())),
    }
}

fn parse_style(
    rule_kind: &str,
    kind: Option<&str>,
    color: Option<&str>,
) -> Result<ValueKind, FilterParseError> {
    match (kind, color) {
        (Some(_), Some(_)) => Err(FilterParseError::ConflictingStyleFields {
            rule_kind: rule_kind.to_string(),
        }),
        (Some(kind), None) => {
            parse_color_kind(kind).map_err(|_| FilterParseError::UnknownKindColor(kind.to_string()))
        }
        (None, Some(color)) => parse_color_kind(color)
            .map_err(|_| FilterParseError::UnknownKindColor(color.to_string())),
        (None, None) => Ok(ValueKind::Normal),
    }
}

fn parse_range_list(input: &str) -> Result<Vec<NumericRangeSegment>, FilterParseError> {
    let mut ranges = Vec::new();

    for part in input.split(',') {
        let part = part.trim();
        if part.is_empty() {
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
                start: start_value,
                end: end_value,
            });
        } else {
            let value = parse_numeric_value(part)?;
            ranges.push(NumericRangeSegment {
                start: value.clone(),
                end: value,
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

fn normalize_runtime_value(value: &VariableValue, num_bits: u32) -> VariableValue {
    match value {
        VariableValue::BigUint(_) => value.clone(),
        VariableValue::String(s) => VariableValue::String(format!(
            "{extra}{body}",
            extra = extend_string(s, num_bits),
            body = s
        )),
    }
}

fn render_runtime_value(value: &VariableValue, num_bits: u32) -> String {
    match value {
        VariableValue::BigUint(_) => key_display(value, num_bits),
        VariableValue::String(s) => s.clone(),
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
    use camino::Utf8PathBuf;
    use surfer_translation_types::BasicTranslator;

    #[test]
    fn parse_value_range_string_and_regex_rules() {
        let content = r##"
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
replace = "Group:$1"
kind = "warn"
"##;

        let parsed = parse_content_with_default_name(content, None).unwrap();
        assert_eq!(parsed.name, "State filter");
        assert_eq!(parsed.bits, Some(4));
        assert_eq!(parsed.rules.len(), 4);
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
    fn translator_applies_first_matching_rule() {
        let content = r#"
name = "State filter"
bits = 4

[[rules]]
rule = "range"
match = "0-15"
replace = "GENERIC"
kind = "warn"

[[rules]]
rule = "value"
match = "0x2"
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
    fn translator_applies_regex_captures_to_rendered_value() {
        let content = r#"
[[rules]]
rule = "regex"
pattern = "^10(..)$"
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
rule = "string"
match = "xx01"
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
        fs::write(
            &path,
            "[[rules]]\nrule = \"value\"\nmatch = \"1\"\nreplace = \"ONE\"\n",
        )
        .unwrap();
        let translator = FilterTranslator::new_from_file(&path).unwrap();
        assert_eq!(translator.name(), "my_filter");
    }

    #[test]
    fn translator_requires_only_one_of_kind_or_color() {
        let content = r##"
[[rules]]
rule = "value"
match = "1"
replace = "ONE"
kind = "warn"
color = "#ff0000"
    "##;

        match parse_content_with_default_name(content, None) {
            Err(FilterParseError::ConflictingStyleFields { .. }) => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }
}
