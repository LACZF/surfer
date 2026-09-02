//! Command prompt handling.
use std::{fs, rc::Rc, str::FromStr};

use camino::Utf8PathBuf;

use crate::config::ArrowKeyBindings;
use crate::displayed_item_tree::{Node, VisibleItemIndex};
use crate::frame_buffer::FrameBufferColorMode;
use crate::fzcmd::{Command, ParamGreed};
use crate::hierarchy::HierarchyStyle;
use crate::message::MessageTarget;
use crate::transaction_container::StreamScopeRef;
use crate::wave_container::{ScopeRef, ScopeRefExt, VariableRef, VariableRefExt};
use crate::wave_data::ScopeType;
use crate::wave_source::LoadOptions;
use crate::{
    SystemState,
    clock_highlighting::ClockHighlightType,
    displayed_item::{
        AddVariableOptions, AnalogRenderStyle, AnalogSettings, AnalogYAxisScale, DisplayedItem,
    },
    message::Message,
    toolbar::toolbar_group_specs,
    util::{alpha_idx_to_uint_idx, uint_idx_to_alpha_idx},
    variable_name_type::VariableNameType,
};
use enum_iterator::Sequence;
use itertools::Itertools;
use tracing::warn;

/// Builds the next command from a complete rest argument.
type RestCommand = Box<dyn Fn(&str) -> Option<Command<Message>>>;

/// Match str with wave file extensions, currently: vcd, fst, ghw
fn is_wave_file_extension(ext: &str) -> bool {
    matches!(ext, "vcd" | "fst" | "ghw")
}

/// Match str with command file extensions, currently: sucl
fn is_command_file_extension(ext: &str) -> bool {
    matches!(ext, "sucl")
}

/// Splits one optional argument while keeping an empty argument visible to completion.
fn optional_at_space_split(query: &str) -> (String, String, String, String) {
    let trimmed = query.trim_start();
    if trimmed.is_empty() {
        return (String::new(), " ".to_string(), String::new(), String::new());
    }

    let leading_len = query.len() - trimmed.len();
    let word_end = trimmed
        .find(|character: char| character.is_whitespace())
        .unwrap_or(trimmed.len());
    if word_end == trimmed.len() {
        return (
            query[..leading_len].to_string(),
            trimmed.to_string(),
            String::new(),
            String::new(),
        );
    }

    let delimiter_len = trimmed[word_end..].chars().next().unwrap().len_utf8();
    (
        query[..leading_len].to_string(),
        trimmed[..word_end].to_string(),
        trimmed[word_end..word_end + delimiter_len].to_string(),
        trimmed[word_end + delimiter_len..].to_string(),
    )
}

/// Keeps an optional command tail together instead of splitting it into words.
fn optional_rest_split(query: &str) -> (String, String, String, String) {
    let trimmed = query.trim_start();
    if trimmed.is_empty() {
        (String::new(), " ".to_string(), String::new(), String::new())
    } else {
        (
            String::new(),
            trimmed.to_string(),
            String::new(),
            String::new(),
        )
    }
}

/// Quotes a value when completion would otherwise split it into multiple arguments.
fn quote_command_value(value: &str) -> String {
    if value.chars().any(char::is_whitespace) {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

/// Removes matching single or double quotes and decodes escaped characters.
fn unquote_command_value(value: &str) -> Option<String> {
    let quote = match value.chars().next() {
        Some('"') => '"',
        Some('\'') => '\'',
        _ => return Some(value.to_string()),
    };
    if !value.ends_with(quote) || value.len() < quote.len_utf8() * 2 {
        return None;
    }

    let inner = &value[quote.len_utf8()..value.len() - quote.len_utf8()];
    let mut unquoted = String::new();
    let mut escaped = false;
    for character in inner.chars() {
        if escaped {
            unquoted.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            unquoted.push(character);
        }
    }
    (!escaped).then_some(unquoted)
}

/// Splits a complete add-command tail while keeping quoted values as one word.
fn split_command_words(query: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut word_started = false;

    for character in query.chars() {
        if let Some(quote_character) = quote {
            if escaped {
                current.push(character);
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote_character {
                quote = None;
            } else {
                current.push(character);
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
            word_started = true;
        } else if character.is_whitespace() {
            if word_started {
                words.push(std::mem::take(&mut current));
                word_started = false;
            }
        } else {
            current.push(character);
            word_started = true;
        }
    }

    if quote.is_some() || escaped {
        return None;
    }
    if word_started {
        words.push(current);
    }
    Some(words)
}

/// Finds the byte offset of an option that is outside a quoted value.
fn find_unquoted_option_start(query: &str) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;

    for (index, character) in query.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        match quote {
            Some(quote_character) => match character {
                '\\' => escaped = true,
                character if character == quote_character => quote = None,
                _ => {}
            },
            None => match character {
                '"' | '\'' => quote = Some(character),
                character if character.is_whitespace() => {
                    let next_index = index + character.len_utf8();
                    if query[next_index..].starts_with('-') {
                        return Some(next_index);
                    }
                }
                _ => {}
            },
        }
    }

    None
}

/// Builds the fuzzy parser tuple from byte offsets within the trimmed input.
fn split_value_parts(
    leading: &str,
    trimmed: &str,
    value_end: usize,
    remainder_start: usize,
) -> (String, String, String, String) {
    let value = trimmed[..value_end].to_string();
    let Some(delimiter) = trimmed[value_end..].chars().next() else {
        return (leading.to_string(), value, String::new(), String::new());
    };

    let delimiter_len = delimiter.len_utf8();
    (
        leading.to_string(),
        value,
        trimmed[value_end..value_end + delimiter_len].to_string(),
        trimmed[remainder_start..].to_string(),
    )
}

/// Splits an option value without breaking known multi-word values or quoted text.
///
/// If a complete known value is not present, the next unquoted option starts the
/// remainder; this lets the add-command state machine offer the following option.
fn separate_value_at_space(query: &str, values: &[String]) -> (String, String, String, String) {
    let trimmed = query.trim_start();
    let leading_len = query.len() - trimmed.len();
    if trimmed.is_empty() {
        return (String::new(), " ".to_string(), String::new(), String::new());
    }
    let leading = &query[..leading_len];

    let matching_value_end = values
        .iter()
        .filter(|value| {
            trimmed
                .strip_prefix(value.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        })
        .map(String::len)
        .max();

    if let Some(value_end) = matching_value_end {
        let remainder_start = value_end
            + trimmed[value_end..]
                .chars()
                .next()
                .map_or(0, char::len_utf8);
        return split_value_parts(leading, trimmed, value_end, remainder_start);
    }

    let (value_end, remainder_start) = match find_unquoted_option_start(trimmed) {
        Some(option_start) => (trimmed[..option_start].trim_end().len(), option_start),
        None => {
            let value_end = trimmed.trim_end().len();
            let remainder_start = value_end
                + trimmed[value_end..]
                    .chars()
                    .next()
                    .map_or(0, char::len_utf8);
            (value_end, remainder_start)
        }
    };

    split_value_parts(leading, trimmed, value_end, remainder_start)
}

#[derive(Clone, Copy)]
/// Describes which creation options apply to an add command.
enum AddOptionSupport {
    Waveform,
    Base,
}

impl AddOptionSupport {
    /// Selects the option set for waveform versus transaction containers.
    const fn from_container_type(is_transaction_container: bool) -> Self {
        if is_transaction_container {
            Self::Base
        } else {
            Self::Waveform
        }
    }

    /// Returns whether an option is meaningful for this item type.
    const fn supports(self, option: AddOption) -> bool {
        matches!(self, Self::Waveform)
            || matches!(
                option,
                AddOption::Color | AddOption::Background | AddOption::Group | AddOption::Position
            )
    }

    /// Parses a complete add-command tail, including positional arguments and options.
    fn parse_add_arguments(
        self,
        query: &str,
        option_values: &AddOptionValues,
    ) -> Option<(Vec<String>, Option<AddVariableOptions>)> {
        let mut state = AddArgumentState::default();
        let mut tokens = split_command_words(query)?.into_iter();

        while let Some(token) = tokens.next() {
            let option = match token.parse::<AddOption>().ok() {
                Some(option) => option,
                None if token.starts_with('-') => return None,
                None => {
                    state.positional.push(token);
                    continue;
                }
            };

            let value = tokens.next()?;
            if value.starts_with('-') {
                return None;
            }
            state.apply_option(option, &value, self, option_values)?;
        }

        Some((state.positional.clone(), state.options_if_present()))
    }
}

/// Parses a finite, strictly positive height multiplier.
fn parse_positive_height(value: &str) -> Option<f32> {
    let height = value.parse::<f32>().ok()?;
    (height.is_finite() && height > 0.0).then_some(height)
}

#[derive(Clone, Default)]
/// Accumulates arguments while the fuzzy parser walks an add command.
struct AddArgumentState {
    positional: Vec<String>,
    options: AddVariableOptions,
}

/// Builds a message from parsed positional arguments and optional add options.
type AddItemMessageBuilder = Rc<dyn Fn(Vec<String>, Option<AddVariableOptions>) -> Option<Message>>;
/// Supplies positional completion values based on the number already entered.
type AddItemPositionalSuggestions = Rc<dyn Fn(usize) -> Vec<String>>;

#[derive(Clone)]
/// Shared values offered by add-command option completion.
struct AddOptionValues {
    format_names: Rc<Vec<String>>,
    color_names: Rc<Vec<String>>,
    group_names: Rc<Vec<String>>,
    height_suggestions: Rc<Vec<String>>,
    analog_suggestions: Rc<Vec<String>>,
    y_axis_suggestions: Rc<Vec<String>>,
}

#[derive(Clone)]
/// Data shared by every state in an add-command parser.
struct AddCommandConfig {
    support: AddOptionSupport,
    positional_suggestions: AddItemPositionalSuggestions,
    option_values: AddOptionValues,
    positional_count: usize,
    message_builder: AddItemMessageBuilder,
}

impl AddCommandConfig {
    /// Returns values that can follow a particular creation option.
    fn option_values(&self, option: AddOption) -> Vec<String> {
        match option {
            AddOption::Format => self
                .option_values
                .format_names
                .iter()
                .map(|name| quote_command_value(name))
                .collect(),
            AddOption::Color | AddOption::Background => self
                .option_values
                .color_names
                .iter()
                .map(|name| quote_command_value(name))
                .collect(),
            AddOption::Height => self.option_values.height_suggestions.as_ref().clone(),
            AddOption::Analog => self.option_values.analog_suggestions.as_ref().clone(),
            AddOption::YAxis => self
                .option_values
                .y_axis_suggestions
                .iter()
                .map(|value| quote_command_value(value))
                .collect(),
            AddOption::Group => self
                .option_values
                .group_names
                .iter()
                .map(|name| quote_command_value(name))
                .collect(),
            AddOption::Position => Vec::new(),
            AddOption::Name => Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Sequence)]
/// The supported creation options.
enum AddOption {
    Format,
    Color,
    Background,
    Height,
    Analog,
    YAxis,
    Group,
    Position,
    Name,
}

impl FromStr for AddOption {
    type Err = ();

    fn from_str(query: &str) -> Result<Self, Self::Err> {
        match query {
            "--format" | "-f" => Ok(Self::Format),
            "--color" | "-c" => Ok(Self::Color),
            "--background" | "-b" => Ok(Self::Background),
            "--height" | "-h" => Ok(Self::Height),
            "--analog" | "-a" => Ok(Self::Analog),
            "--yaxis" | "-y" => Ok(Self::YAxis),
            "--group" | "-g" => Ok(Self::Group),
            "--position" | "-p" => Ok(Self::Position),
            "--name" | "-n" => Ok(Self::Name),
            _ => Err(()),
        }
    }
}

impl AddOption {
    /// Returns the command-line key for this optional argument.
    const fn optional_argument_key(self) -> &'static str {
        match self {
            Self::Format => "--format",
            Self::Color => "--color",
            Self::Background => "--background",
            Self::Height => "--height",
            Self::Analog => "--analog",
            Self::YAxis => "--yaxis",
            Self::Group => "--group",
            Self::Position => "--position",
            Self::Name => "--name",
        }
    }
}

impl AddArgumentState {
    /// Returns whether an option has already been supplied in this parser state.
    fn is_set(&self, option: AddOption) -> bool {
        match option {
            AddOption::Format => self.options.format.is_some(),
            AddOption::Color => self.options.color.is_some(),
            AddOption::Background => self.options.background_color.is_some(),
            AddOption::Height => self.options.height.is_some(),
            AddOption::Analog => self.options.analog.is_some(),
            AddOption::YAxis => self.options.y_axis.is_some(),
            AddOption::Group => self.options.group.is_some(),
            AddOption::Position => self.options.position.is_some(),
            AddOption::Name => self.options.name.is_some(),
        }
    }

    /// Applies and validates one add-command option value.
    fn apply_option(
        &mut self,
        option: AddOption,
        value: &str,
        support: AddOptionSupport,
        option_values: &AddOptionValues,
    ) -> Option<()> {
        if !support.supports(option) || self.is_set(option) {
            return None;
        }

        match option {
            AddOption::Format => {
                if !option_values.format_names.iter().any(|name| name == value) {
                    return None;
                }
                self.options.format = Some(value.to_string());
            }
            AddOption::Color => {
                if !option_values.color_names.iter().any(|name| name == value) {
                    return None;
                }
                self.options.color = Some(value.to_string());
            }
            AddOption::Background => {
                if !option_values.color_names.iter().any(|name| name == value) {
                    return None;
                }
                self.options.background_color = Some(value.to_string());
            }
            AddOption::Height => {
                self.options.height = Some(parse_positive_height(value)?);
            }
            AddOption::Analog => {
                self.options.analog = Some(value.parse().ok()?);
            }
            AddOption::YAxis => {
                self.options.y_axis = Some(value.parse().ok()?);
            }
            AddOption::Group => self.options.group = Some(value.to_string()),
            AddOption::Position => self.options.position = Some(value.parse().ok()?),
            AddOption::Name => self.options.name = Some(value.to_string()),
        }

        Some(())
    }

    /// Returns creation options when at least one option was supplied.
    fn options_if_present(&self) -> Option<AddVariableOptions> {
        enum_iterator::all::<AddOption>()
            .any(|option| self.is_set(option))
            .then(|| self.options.clone())
    }

    /// Combines positional suggestions with options still available in the state.
    fn command_suggestions(&self, config: &AddCommandConfig) -> Vec<String> {
        let mut suggestions = if self.positional.len() < config.positional_count {
            (config.positional_suggestions)(self.positional.len())
        } else {
            Vec::new()
        };
        suggestions.extend(
            enum_iterator::all::<AddOption>()
                .filter(|option| config.support.supports(*option) && !self.is_set(*option))
                .map(|option| option.optional_argument_key().to_string()),
        );
        suggestions
    }

    /// Creates the next state in the add-command parser.
    ///
    /// A completed positional argument or option value returns to this state so
    /// another option can be entered before the command is committed.
    fn argument_command(self, config: AddCommandConfig) -> Command<Message> {
        let suggestions = self.command_suggestions(&config);
        let parser_state = self.clone();
        Command::NonTerminal(
            ParamGreed::Custom(&optional_at_space_split),
            suggestions,
            Box::new(move |query, _| {
                let argument = query.trim();
                if argument.is_empty() {
                    if parser_state.positional.len() < config.positional_count {
                        return None;
                    }
                    return (config.message_builder)(
                        parser_state.positional.clone(),
                        parser_state.options_if_present(),
                    )
                    .map(Command::Terminal);
                }

                if let Ok(option) = argument.parse::<AddOption>() {
                    return parser_state
                        .clone()
                        .option_value_command(option, config.clone());
                }
                if argument.starts_with('-')
                    || parser_state.positional.len() >= config.positional_count
                {
                    return None;
                }

                let mut next_state = parser_state.clone();
                next_state.positional.push(argument.to_string());
                Some(next_state.argument_command(config.clone()))
            }),
        )
    }

    /// Creates the parser state for an option's value and validates that value.
    fn option_value_command(
        self,
        option: AddOption,
        config: AddCommandConfig,
    ) -> Option<Command<Message>> {
        if !config.support.supports(option) || self.is_set(option) {
            return None;
        }

        let suggestions = config.option_values(option);
        Some(Command::NonTerminal(
            ParamGreed::CustomOwned(Box::new({
                let values = suggestions.clone();
                move |query| separate_value_at_space(query, &values)
            })),
            suggestions,
            Box::new(move |query, _| {
                let value = unquote_command_value(query.trim())?;
                if value.is_empty() || value.starts_with('-') {
                    return None;
                }

                let mut next_state = self.clone();
                next_state.apply_option(option, &value, config.support, &config.option_values)?;
                Some(next_state.argument_command(config.clone()))
            }),
        ))
    }
}

/// Builds the command parser for the current application state.
pub(crate) fn get_parser(state: &SystemState) -> Command<Message> {
    /// Creates a parser for a rest parameter with fixed suggestions.
    fn single_word(
        suggestions: Vec<String>,
        rest_command: RestCommand,
    ) -> Option<Command<Message>> {
        Some(Command::NonTerminal(
            ParamGreed::Rest,
            suggestions,
            Box::new(move |query, _| rest_command(query)),
        ))
    }

    /// Creates a parser for an optional word with fixed suggestions.
    fn optional_single_word(
        suggestions: Vec<String>,
        rest_command: RestCommand,
    ) -> Option<Command<Message>> {
        Some(Command::NonTerminal(
            ParamGreed::OptionalWord,
            suggestions,
            Box::new(move |query, _| rest_command(query)),
        ))
    }

    /// Creates a parser whose suggestions are calculated when the command is selected.
    fn single_word_delayed_suggestions(
        suggestions: Box<dyn Fn() -> Vec<String>>,
        rest_command: RestCommand,
    ) -> Option<Command<Message>> {
        Some(Command::NonTerminal(
            ParamGreed::Rest,
            suggestions(),
            Box::new(move |query, _| rest_command(query)),
        ))
    }

    let scopes = match &state.user.waves {
        Some(v) => v.inner.scope_names(),
        None => vec![],
    };
    let variables = match &state.user.waves {
        Some(v) => v.inner.variable_names(),
        None => vec![],
    };
    let arrays = match &state.user.waves {
        Some(v) => v.inner.array_names(),
        None => vec![],
    };
    let surver_file_names = state
        .user
        .surver_file_infos
        .as_ref()
        .map_or(vec![], |file_infos| {
            file_infos
                .iter()
                .map(|info| info.filename.clone())
                .collect()
        });
    let displayed_items = match &state.user.waves {
        Some(v) => v
            .items_tree
            .iter_visible()
            .enumerate()
            .map(
                |(
                    vidx,
                    Node {
                        item_ref: item_id, ..
                    },
                )| {
                    let idx = VisibleItemIndex(vidx);
                    let item = &v.displayed_items[item_id];
                    match item {
                        DisplayedItem::Variable(var) => format!(
                            "{}_{}",
                            uint_idx_to_alpha_idx(idx, v.displayed_items.len()),
                            var.variable_ref.full_path_string()
                        ),
                        _ => format!(
                            "{}_{}",
                            uint_idx_to_alpha_idx(idx, v.displayed_items.len()),
                            item.name()
                        ),
                    }
                },
            )
            .collect_vec(),
        None => vec![],
    };
    let group_names = state
        .user
        .waves
        .as_ref()
        .map(|waves| {
            waves
                .items_tree
                .iter()
                .filter_map(|node| match waves.displayed_items.get(&node.item_ref) {
                    Some(DisplayedItem::Group(group)) => Some(group.name.clone()),
                    _ => None,
                })
                .collect_vec()
        })
        .unwrap_or_default();
    let variables_in_active_scope = state
        .user
        .waves
        .as_ref()
        .and_then(|waves| {
            waves
                .active_scope
                .as_ref()
                .map(|scope| waves.inner.variables_in_scope(scope))
        })
        .unwrap_or_default();

    let color_names = state.user.config.theme.colors.keys().cloned().collect_vec();
    let format_names: Vec<String> = state
        .translators
        .all_translator_names()
        .into_iter()
        .map(&str::to_owned)
        .collect();
    let height_suggestions = state
        .user
        .config
        .layout
        .waveforms_line_height_multiples
        .iter()
        .map(ToString::to_string)
        .collect_vec();
    let analog_suggestions = vec!["step".to_string(), "interpolated".to_string()];
    let y_axis_suggestions = enum_iterator::all::<AnalogYAxisScale>()
        .map(|scale| scale.label().to_ascii_lowercase())
        .collect_vec();
    let option_values = AddOptionValues {
        format_names: Rc::new(format_names.clone()),
        color_names: Rc::new(color_names.clone()),
        group_names: Rc::new(group_names.clone()),
        height_suggestions: Rc::new(height_suggestions.clone()),
        analog_suggestions: Rc::new(analog_suggestions),
        y_axis_suggestions: Rc::new(y_axis_suggestions),
    };
    let active_scope = state
        .user
        .waves
        .as_ref()
        .and_then(|w| w.active_scope.clone());

    let is_transaction_container = state
        .user
        .waves
        .as_ref()
        .is_some_and(|w| w.inner.is_transactions());

    /// Lists files in the current directory whose extension matches the predicate.
    fn files_with_ext(matches: fn(&str) -> bool) -> Vec<String> {
        if let Ok(res) = fs::read_dir(".") {
            res.map(|res| res.map(|e| e.path()).unwrap_or_default())
                .filter(|file| {
                    file.extension()
                        .is_some_and(|extension| (matches)(extension.to_str().unwrap_or("")))
                })
                .map(|file| file.into_os_string().into_string().unwrap())
                .collect::<Vec<String>>()
        } else {
            vec![]
        }
    }

    /// Returns wave files available for loading.
    fn all_wave_files() -> Vec<String> {
        files_with_ext(is_wave_file_extension)
    }

    /// Returns command files available for loading.
    fn all_command_files() -> Vec<String> {
        files_with_ext(is_command_file_extension)
    }

    let timescale = state
        .user
        .waves
        .as_ref()
        .map(|w| w.inner.metadata().timescale.clone());
    let cursor = state
        .user
        .waves
        .as_ref()
        .and_then(|waves| waves.cursor.clone());

    let viewport_idx = state
        .user
        .waves
        .as_ref()
        .map_or(0, |waves| waves.last_active_viewport_idx);
    let viewport_indices = state.user.waves.as_ref().map_or(vec![], |waves| {
        (0..waves.viewports.len())
            .map(|idx| idx.to_string())
            .collect()
    });

    let markers = if let Some(waves) = &state.user.waves {
        waves
            .items_tree
            .iter()
            .map(|Node { item_ref, .. }| waves.displayed_items.get(item_ref))
            .filter_map(|item| match item {
                Some(DisplayedItem::Marker(marker)) => Some((marker.name.clone(), marker.idx)),
                _ => None,
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    /// Resolves a marker name or numeric marker reference.
    fn parse_marker(query: &str, markers: &[(Option<String>, u8)]) -> Option<u8> {
        if let Some(id_str) = query.strip_prefix("#") {
            let id = id_str.parse::<u8>().ok()?;
            Some(id)
        } else {
            markers
                .iter()
                .find_map(|(name, idx)| name.as_ref().and_then(|n| (n == query).then_some(*idx)))
        }
    }

    /// Builds completion values for marker names and numeric references.
    fn marker_suggestions(markers: &[(Option<String>, u8)]) -> Vec<String> {
        markers
            .iter()
            .flat_map(|(name, idx)| {
                [name.clone(), Some(format!("#{idx}"))]
                    .into_iter()
                    .flatten()
            })
            .collect()
    }

    let wcp_start_or_stop = if state
        .wcp_running_signal
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        "wcp_server_stop"
    } else {
        "wcp_server_start"
    };
    #[cfg(target_arch = "wasm32")]
    let _ = wcp_start_or_stop;

    let keep_during_reload = state.user.config.behavior.keep_during_reload;
    let toolbar_group_ids = toolbar_group_specs()
        .iter()
        .map(|spec| spec.id.to_string())
        .collect_vec();
    let mut commands = if state.user.waves.is_some() {
        vec![
            "load_file",
            "load_url",
            #[cfg(not(target_arch = "wasm32"))]
            "load_state",
            "run_command_file",
            "run_command_file_from_url",
            "switch_file",
            "variable_add",
            "generator_add",
            "item_focus",
            "item_set_color",
            "item_set_background_color",
            "item_set_format",
            "item_set_height",
            "item_set_analog",
            "item_unset_color",
            "item_unset_background_color",
            "item_unfocus",
            "item_rename",
            "zoom_fit",
            "scope_add",
            #[cfg(not(target_arch = "wasm32"))]
            "create_default_config",
            "scope_add_recursive",
            "scope_add_as_group",
            "scope_add_as_group_recursive",
            "scope_select",
            "scope_select_root",
            "stream_add",
            "stream_select",
            "stream_select_root",
            "divider_add",
            "config_reload",
            "theme_select",
            "reload",
            "remove_unavailable",
            "show_controls",
            "show_mouse_gestures",
            "show_quick_start",
            "show_logs",
            #[cfg(feature = "performance_plot")]
            "show_performance",
            "scroll_to_start",
            "scroll_to_end",
            "goto_start",
            "goto_end",
            "zoom_in",
            "zoom_out",
            "zoom_to",
            "toggle_menu",
            "toggle_side_panel",
            "toggle_fullscreen",
            "toggle_tick_lines",
            "toolbar_set_visible",
            "toolbar_set_row",
            "variable_add_from_scope",
            "generator_add_from_stream",
            "variable_set_name_type",
            "variable_force_name_type",
            "preference_set_clock_highlight",
            "preference_set_hierarchy_style",
            "preference_set_arrow_key_bindings",
            "goto_cursor",
            "goto_marker",
            "dump_tree",
            "group_marked",
            "group_add",
            "group_dissolve",
            "group_fold_recursive",
            "group_unfold_recursive",
            "group_fold_all",
            "group_unfold_all",
            "save_state",
            "save_state_as",
            "timeline_add",
            "cursor_set",
            "goto_time",
            "marker_set",
            "marker_set_at",
            "marker_remove",
            "show_marker_window",
            "viewport_add",
            "viewport_remove",
            "viewport_set_active",
            "transition_next",
            "transition_previous",
            "transaction_next",
            "transaction_prev",
            "copy_value",
            "frame_buffer_set_array",
            "frame_buffer_set_variable",
            "frame_buffer_set_mode",
            "frame_buffer_set_width",
            "frame_buffer_set_range",
            "memory_viewer_open",
            "pause_simulation",
            "unpause_simulation",
            "undo",
            "redo",
            #[cfg(not(target_arch = "wasm32"))]
            wcp_start_or_stop,
            #[cfg(not(target_arch = "wasm32"))]
            "exit",
        ]
    } else {
        vec![
            "load_file",
            "load_url",
            #[cfg(not(target_arch = "wasm32"))]
            "load_state",
            "run_command_file",
            "run_command_file_from_url",
            "config_reload",
            "theme_select",
            "toggle_menu",
            "toggle_side_panel",
            "toggle_fullscreen",
            "toolbar_set_visible",
            "toolbar_set_row",
            "preference_set_clock_highlight",
            "preference_set_hierarchy_style",
            "preference_set_arrow_key_bindings",
            "show_controls",
            "show_mouse_gestures",
            "show_quick_start",
            "show_logs",
            #[cfg(not(target_arch = "wasm32"))]
            "create_default_config",
            #[cfg(feature = "performance_plot")]
            "show_performance",
            #[cfg(not(target_arch = "wasm32"))]
            wcp_start_or_stop,
            #[cfg(not(target_arch = "wasm32"))]
            "exit",
        ]
    };
    if !surver_file_names.is_empty() {
        commands.push("surver_select_file");
        commands.push("surver_switch_file");
    }

    let mut theme_names = state.user.config.theme.theme_names.clone();
    let state_file = state.user.state_file.clone();
    let show_hierarchy = state.show_hierarchy();
    let show_menu = state.show_menu();
    let show_tick_lines = state.show_ticks();
    theme_names.insert(0, "default".to_string());
    Command::NonTerminal(
        ParamGreed::Word,
        commands.into_iter().map(std::convert::Into::into).collect(),
        Box::new(move |query, _| {
            let variables_in_active_scope = variables_in_active_scope.clone();
            let markers = markers.clone();
            let cursor = cursor.clone();
            let scopes = scopes.clone();
            let active_scope = active_scope.clone();
            let is_transaction_container = is_transaction_container;
            match query {
                "load_file" => single_word_delayed_suggestions(
                    Box::new(all_wave_files),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::LoadFile(
                            word.into(),
                            LoadOptions::Clear,
                        )))
                    }),
                ),

                "create_default_config" => Some(Command::Terminal(Message::DownloadDefaultConfig)),
                "switch_file" => single_word_delayed_suggestions(
                    Box::new(all_wave_files),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::LoadFile(
                            word.into(),
                            LoadOptions::KeepAll,
                        )))
                    }),
                ),
                "load_url" => Some(Command::NonTerminal(
                    ParamGreed::Rest,
                    vec![],
                    Box::new(|query, _| {
                        Some(Command::Terminal(Message::LoadWaveformFileFromUrl(
                            query.to_string(),
                            LoadOptions::Clear, // load_url does not indicate any format restrictions
                        )))
                    }),
                )),
                "run_command_file" => single_word_delayed_suggestions(
                    Box::new(all_command_files),
                    Box::new(|word| Some(Command::Terminal(Message::LoadCommandFile(word.into())))),
                ),
                "run_command_file_from_url" => Some(Command::NonTerminal(
                    ParamGreed::Rest,
                    vec![],
                    Box::new(|query, _| {
                        Some(Command::Terminal(Message::LoadCommandFileFromUrl(
                            query.to_string(),
                        )))
                    }),
                )),
                "config_reload" => Some(Command::Terminal(Message::ReloadConfig)),
                "theme_select" => single_word(
                    theme_names.clone(),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::SelectTheme(Some(
                            word.to_owned(),
                        ))))
                    }),
                ),
                "scroll_to_start" | "goto_start" => {
                    Some(Command::Terminal(Message::GoToStart { viewport_idx }))
                }
                "scroll_to_end" | "goto_end" => {
                    Some(Command::Terminal(Message::GoToEnd { viewport_idx }))
                }
                "zoom_in" => Some(Command::Terminal(Message::CanvasZoom {
                    mouse_ptr: None,
                    delta: 0.5,
                    viewport_idx,
                })),
                "zoom_out" => Some(Command::Terminal(Message::CanvasZoom {
                    mouse_ptr: None,
                    delta: 2.0,
                    viewport_idx,
                })),
                "zoom_fit" => Some(Command::Terminal(Message::ZoomToFit { viewport_idx })),
                "zoom_to" => {
                    let timescale_for_zoom = timescale.clone();
                    Some(Command::NonTerminal(
                        ParamGreed::Rest,
                        vec![],
                        Box::new(move |params, _| {
                            let parts: Vec<&str> = params.split_whitespace().collect();
                            if parts.len() < 2 {
                                return None;
                            }

                            // Reconstruct time strings, handling optional spaces between number and unit
                            let mut time_strings = Vec::new();
                            let mut i = 0;
                            while i < parts.len() && time_strings.len() < 2 {
                                let part = parts[i];
                                // Check if this part is numeric (potentially followed by a unit in the next part)
                                if part
                                    .chars()
                                    .next()
                                    .is_some_and(|c| c.is_numeric() || c == '-')
                                {
                                    let time_str = if i + 1 < parts.len()
                                        && !parts[i + 1].chars().next().unwrap_or('0').is_numeric()
                                    {
                                        // Next part looks like a unit, combine them
                                        let combined = format!("{}{}", part, parts[i + 1]);
                                        i += 2;
                                        combined
                                    } else {
                                        // No unit following, use as-is
                                        i += 1;
                                        part.to_string()
                                    };
                                    time_strings.push(time_str);
                                } else {
                                    i += 1;
                                }
                            }

                            if time_strings.len() < 2 {
                                return None;
                            }

                            let start_time = if let Some(ts) = &timescale_for_zoom {
                                crate::time::parse_time_string_to_ticks(&time_strings[0], ts)?
                            } else {
                                time_strings[0].parse().ok()?
                            };

                            let end_time = if let Some(ts) = &timescale_for_zoom {
                                crate::time::parse_time_string_to_ticks(&time_strings[1], ts)?
                            } else {
                                time_strings[1].parse().ok()?
                            };

                            Some(Command::Terminal(Message::ZoomToRange {
                                start: start_time,
                                end: end_time,
                                viewport_idx,
                            }))
                        }),
                    ))
                }
                "toggle_menu" => Some(Command::Terminal(Message::SetMenuVisible(!show_menu))),
                "toggle_side_panel" => Some(Command::Terminal(Message::SetSidePanelVisible(
                    !show_hierarchy,
                ))),
                "toggle_fullscreen" => Some(Command::Terminal(Message::ToggleFullscreen)),
                "toggle_tick_lines" => {
                    Some(Command::Terminal(Message::SetTickLines(!show_tick_lines)))
                }
                "toolbar_set_visible" => Some(Command::NonTerminal(
                    ParamGreed::Word,
                    toolbar_group_ids.clone(),
                    Box::new({
                        let toolbar_group_ids = toolbar_group_ids.clone();
                        move |word, _| {
                            if !toolbar_group_ids.iter().any(|id| id == word) {
                                return None;
                            }
                            let group_id = word.to_string();
                            Some(Command::NonTerminal(
                                ParamGreed::Word,
                                vec!["true".to_string(), "false".to_string()],
                                Box::new(move |value, _| {
                                    let enabled = match value {
                                        "true" => true,
                                        "false" => false,
                                        _ => return None,
                                    };
                                    Some(Command::Terminal(Message::SetToolbarGroupEnabled(
                                        group_id.clone(),
                                        enabled,
                                    )))
                                }),
                            ))
                        }
                    }),
                )),
                "toolbar_set_row" => Some(Command::NonTerminal(
                    ParamGreed::Word,
                    toolbar_group_ids.clone(),
                    Box::new({
                        let toolbar_group_ids = toolbar_group_ids.clone();
                        move |word, _| {
                            if !toolbar_group_ids.iter().any(|id| id == word) {
                                return None;
                            }
                            let group_id = word.to_string();
                            Some(Command::NonTerminal(
                                ParamGreed::Word,
                                vec![],
                                Box::new(move |value, _| {
                                    let row = value.parse::<u8>().ok()?;
                                    Some(Command::Terminal(Message::SetToolbarGroupRow(
                                        group_id.clone(),
                                        row,
                                    )))
                                }),
                            ))
                        }
                    }),
                )),
                // scope commands
                "scope_add" | "module_add" | "stream_add" | "scope_add_recursive" => {
                    let recursive = query == "scope_add_recursive";
                    let support = AddOptionSupport::from_container_type(is_transaction_container);
                    let positional_suggestions = Rc::new({
                        let scopes = scopes.clone();
                        move |_| scopes.clone()
                    });
                    let message_builder: AddItemMessageBuilder = if is_transaction_container {
                        if recursive {
                            warn!("Cannot recursively add transaction containers");
                        }
                        Rc::new(|parts, options| {
                            if parts.len() != 1 {
                                return None;
                            }
                            Some(Message::AddAllFromStreamScope(
                                parts[0].clone(),
                                options.map(|o| o.as_item_options()),
                            ))
                        })
                    } else {
                        Rc::new(move |parts, options| {
                            if parts.len() != 1 {
                                return None;
                            }
                            Some(Message::AddScope(
                                ScopeRef::from_hierarchy_string(&parts[0]),
                                recursive,
                                options,
                            ))
                        })
                    };
                    Some(
                        AddArgumentState::default().argument_command(AddCommandConfig {
                            support,
                            positional_suggestions,
                            option_values: option_values.clone(),
                            positional_count: 1,
                            message_builder,
                        }),
                    )
                }
                "scope_add_as_group" | "scope_add_as_group_recursive" => {
                    let recursive = query == "scope_add_as_group_recursive";
                    if is_transaction_container {
                        warn!("Cannot add transaction containers as group");
                        None
                    } else {
                        let positional_suggestions = Rc::new({
                            let scopes = scopes.clone();
                            move |_| scopes.clone()
                        });
                        let message_builder: AddItemMessageBuilder =
                            Rc::new(move |parts, options| {
                                if parts.len() != 1 {
                                    return None;
                                }
                                Some(Message::AddScopeAsGroup(
                                    ScopeRef::from_hierarchy_string(&parts[0]),
                                    recursive,
                                    options,
                                ))
                            });
                        Some(
                            AddArgumentState::default().argument_command(AddCommandConfig {
                                support: AddOptionSupport::Waveform,
                                positional_suggestions,
                                option_values: option_values.clone(),
                                positional_count: 1,
                                message_builder,
                            }),
                        )
                    }
                }
                "scope_select" | "stream_select" => {
                    if is_transaction_container {
                        single_word(
                            scopes.clone(),
                            Box::new(|word| {
                                let scope = if word == "tr" {
                                    ScopeType::StreamScope(StreamScopeRef::Root)
                                } else {
                                    ScopeType::StreamScope(StreamScopeRef::Empty(word.to_string()))
                                };
                                Some(Command::Terminal(Message::SetActiveScope(Some(scope))))
                            }),
                        )
                    } else {
                        single_word(
                            scopes.clone(),
                            Box::new(|word| {
                                Some(Command::Terminal(Message::SetActiveScope(Some(
                                    ScopeType::WaveScope(ScopeRef::from_hierarchy_string(word)),
                                ))))
                            }),
                        )
                    }
                }
                "scope_select_root" | "stream_select_root" => {
                    Some(Command::Terminal(Message::SetActiveScope(None)))
                }
                "reload" => Some(Command::Terminal(Message::ReloadWaveform(
                    keep_during_reload,
                ))),
                "remove_unavailable" => Some(Command::Terminal(Message::RemovePlaceholders)),
                "surver_select_file" => single_word(
                    surver_file_names.clone(),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::LoadSurverFileByName(
                            word.to_string(),
                            LoadOptions::Clear,
                        )))
                    }),
                ),
                "surver_switch_file" => single_word(
                    surver_file_names.clone(),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::LoadSurverFileByName(
                            word.to_string(),
                            LoadOptions::KeepAll,
                        )))
                    }),
                ),
                // Variable commands
                "variable_add" | "generator_add" => {
                    let support = AddOptionSupport::from_container_type(is_transaction_container);
                    let positional_suggestions = Rc::new({
                        let variables = variables.clone();
                        move |_| variables.clone()
                    });
                    let message_builder: AddItemMessageBuilder = if is_transaction_container {
                        Rc::new(|parts, options| {
                            if parts.len() != 1 {
                                return None;
                            }
                            Some(Message::AddStreamOrGeneratorFromName(
                                None,
                                parts[0].clone(),
                                options.map(|o| o.as_item_options()),
                            ))
                        })
                    } else {
                        Rc::new(|parts, options| {
                            if parts.len() != 1 {
                                return None;
                            }
                            Some(Message::AddVariables(
                                vec![VariableRef::from_hierarchy_string(&parts[0])],
                                options,
                            ))
                        })
                    };
                    Some(
                        AddArgumentState::default().argument_command(AddCommandConfig {
                            support,
                            positional_suggestions,
                            option_values: option_values.clone(),
                            positional_count: 1,
                            message_builder,
                        }),
                    )
                }
                "variable_add_from_scope" | "generator_add_from_stream" => {
                    let support = AddOptionSupport::from_container_type(is_transaction_container);
                    let positional_suggestions = Rc::new({
                        let variables = variables_in_active_scope
                            .iter()
                            .map(|variable| variable.name_with_index())
                            .collect_vec();
                        move |_| variables.clone()
                    });
                    let active_scope = active_scope.clone();
                    let message_builder: AddItemMessageBuilder = Rc::new(move |parts, options| {
                        if parts.len() != 1 {
                            return None;
                        }
                        active_scope.as_ref().map(|scope| match scope {
                            ScopeType::WaveScope(w) => Message::AddVariables(
                                vec![VariableRef::new(w.clone(), parts[0].clone())],
                                options,
                            ),
                            ScopeType::StreamScope(stream_scope) => {
                                Message::AddStreamOrGeneratorFromName(
                                    Some(stream_scope.clone()),
                                    parts[0].clone(),
                                    options.map(|o| o.as_item_options()),
                                )
                            }
                        })
                    });
                    Some(
                        AddArgumentState::default().argument_command(AddCommandConfig {
                            support,
                            positional_suggestions,
                            option_values: option_values.clone(),
                            positional_count: 1,
                            message_builder,
                        }),
                    )
                }
                "item_set_color" => single_word(
                    color_names.clone(),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::ItemColorChange(
                            MessageTarget::CurrentSelection,
                            Some(word.to_string()),
                        )))
                    }),
                ),
                "item_set_background_color" => single_word(
                    color_names.clone(),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::ItemBackgroundColorChange(
                            MessageTarget::CurrentSelection,
                            Some(word.to_string()),
                        )))
                    }),
                ),
                "item_set_format" => single_word(
                    format_names.clone(),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::VariableFormatChange(
                            MessageTarget::CurrentSelection,
                            word.to_string(),
                        )))
                    }),
                ),
                "item_set_height" => single_word(
                    height_suggestions.clone(),
                    Box::new(|word| {
                        let height = parse_positive_height(word)?;
                        Some(Command::Terminal(Message::ItemHeightScalingFactorChange(
                            MessageTarget::CurrentSelection,
                            height,
                        )))
                    }),
                ),
                "item_set_analog" => {
                    let mut suggestions = vec!["off".to_string()];
                    suggestions.extend(option_values.analog_suggestions.iter().cloned());
                    single_word(
                        suggestions,
                        Box::new(|word| {
                            let settings =
                                AnalogRenderStyle::parse_from_str(word)?.map(|render_style| {
                                    AnalogSettings {
                                        render_style,
                                        ..Default::default()
                                    }
                                });

                            Some(Command::Terminal(Message::SetAnalogSettings(
                                MessageTarget::CurrentSelection,
                                settings,
                            )))
                        }),
                    )
                }
                "item_unset_color" => Some(Command::Terminal(Message::ItemColorChange(
                    MessageTarget::CurrentSelection,
                    None,
                ))),
                "item_unset_background_color" => Some(Command::Terminal(
                    Message::ItemBackgroundColorChange(MessageTarget::CurrentSelection, None),
                )),
                "item_rename" => Some(Command::NonTerminal(
                    ParamGreed::Rest,
                    vec![],
                    Box::new(|query, _| {
                        Some(Command::Terminal(Message::ItemNameChange(
                            None,
                            Some(query.to_owned()),
                        )))
                    }),
                )),
                "variable_set_name_type" => single_word(
                    vec![
                        "Local".to_string(),
                        "Unique".to_string(),
                        "Global".to_string(),
                    ],
                    Box::new(|word| {
                        Some(Command::Terminal(Message::ChangeVariableNameType(
                            MessageTarget::CurrentSelection,
                            VariableNameType::from_str(word).unwrap_or(VariableNameType::Local),
                        )))
                    }),
                ),
                "variable_force_name_type" => single_word(
                    vec![
                        "Local".to_string(),
                        "Unique".to_string(),
                        "Global".to_string(),
                    ],
                    Box::new(|word| {
                        Some(Command::Terminal(Message::ForceVariableNameTypes(
                            VariableNameType::from_str(word).unwrap_or(VariableNameType::Local),
                        )))
                    }),
                ),
                "item_focus" => single_word(
                    displayed_items.clone(),
                    Box::new(|word| {
                        // split off the idx which is always followed by an underscore
                        let alpha_idx: String = word.chars().take_while(|c| *c != '_').collect();
                        alpha_idx_to_uint_idx(&alpha_idx)
                            .map(|idx| Command::Terminal(Message::FocusItem(idx)))
                    }),
                ),
                "transition_next" => single_word(
                    displayed_items.clone(),
                    Box::new(|word| {
                        // split off the idx which is always followed by an underscore
                        let alpha_idx: String = word.chars().take_while(|c| *c != '_').collect();
                        alpha_idx_to_uint_idx(&alpha_idx).map(|idx| {
                            Command::Terminal(Message::MoveCursorToTransition {
                                next: true,
                                variable: Some(idx),
                                skip_zero: false,
                            })
                        })
                    }),
                ),
                "transition_previous" => single_word(
                    displayed_items.clone(),
                    Box::new(|word| {
                        // split off the idx which is always followed by an underscore
                        let alpha_idx: String = word.chars().take_while(|c| *c != '_').collect();
                        alpha_idx_to_uint_idx(&alpha_idx).map(|idx| {
                            Command::Terminal(Message::MoveCursorToTransition {
                                next: false,
                                variable: Some(idx),
                                skip_zero: false,
                            })
                        })
                    }),
                ),
                "transaction_next" => {
                    Some(Command::Terminal(Message::MoveTransaction { next: true }))
                }
                "transaction_prev" => {
                    Some(Command::Terminal(Message::MoveTransaction { next: false }))
                }
                "copy_value" => single_word(
                    displayed_items.clone(),
                    Box::new(|word| {
                        // split off the idx which is always followed by an underscore
                        let alpha_idx: String = word.chars().take_while(|c| *c != '_').collect();
                        alpha_idx_to_uint_idx(&alpha_idx).map(|idx| {
                            Command::Terminal(Message::VariableValueToClipbord(
                                MessageTarget::Explicit(idx),
                            ))
                        })
                    }),
                ),
                "preference_set_clock_highlight" => single_word(
                    enum_iterator::all::<ClockHighlightType>()
                        .map(|o| o.to_string())
                        .collect_vec(),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::SetClockHighlightType(
                            ClockHighlightType::from_str(word).unwrap_or(ClockHighlightType::Line),
                        )))
                    }),
                ),
                "preference_set_hierarchy_style" => single_word(
                    enum_iterator::all::<HierarchyStyle>()
                        .map(|o| o.to_string())
                        .collect_vec(),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::SetHierarchyStyle(
                            HierarchyStyle::from_str(word).unwrap_or(HierarchyStyle::Separate),
                        )))
                    }),
                ),
                "preference_set_arrow_key_bindings" => single_word(
                    enum_iterator::all::<ArrowKeyBindings>()
                        .map(|o| o.to_string())
                        .collect_vec(),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::SetArrowKeyBindings(
                            ArrowKeyBindings::from_str(word).unwrap_or(ArrowKeyBindings::Edge),
                        )))
                    }),
                ),
                "item_unfocus" => Some(Command::Terminal(Message::UnfocusItem)),
                "divider_add" => {
                    let option_values = option_values.clone();
                    Some(Command::NonTerminal(
                        ParamGreed::Custom(&optional_rest_split),
                        vec![],
                        Box::new(move |query, _| {
                            let (parts, options) = AddOptionSupport::Base
                                .parse_add_arguments(query, &option_values)?;
                            if parts.len() > 1 {
                                return None;
                            }
                            Some(Command::Terminal(Message::AddDivider(
                                parts.into_iter().next(),
                                None,
                                options.map(|o| o.as_item_options()),
                            )))
                        }),
                    ))
                }
                "timeline_add" => {
                    let positional_suggestions: AddItemPositionalSuggestions =
                        Rc::new(|_| Vec::new());
                    let message_builder: AddItemMessageBuilder = Rc::new(|parts, options| {
                        if !parts.is_empty() {
                            return None;
                        }
                        Some(Message::AddTimeLine(
                            None,
                            options.map(|o| o.as_item_options()),
                        ))
                    });
                    Some(
                        AddArgumentState::default().argument_command(AddCommandConfig {
                            support: AddOptionSupport::Base,
                            positional_suggestions,
                            option_values: option_values.clone(),
                            positional_count: 0,
                            message_builder,
                        }),
                    )
                }
                "goto_cursor" => Some(Command::Terminal(Message::GoToCursorIfNotInView)),
                "goto_marker" => single_word(
                    marker_suggestions(&markers),
                    Box::new(move |name| {
                        parse_marker(name, &markers)
                            .map(|idx| Command::Terminal(Message::GoToMarkerPosition(idx, 0)))
                    }),
                ),
                "frame_buffer_set_array" => single_word(
                    arrays.clone(),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::SetFrameBufferArray(
                            ScopeRef::from_hierarchy_string(word),
                        )))
                    }),
                ),
                "frame_buffer_set_variable" => single_word(
                    variables.clone(),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::SetFrameBufferVariable(
                            VariableRef::from_hierarchy_string(word),
                        )))
                    }),
                ),
                "frame_buffer_set_mode" => Some(Command::NonTerminal(
                    ParamGreed::Word,
                    vec![
                        "grayscale".to_string(),
                        "rgb".to_string(),
                        "ycbcr".to_string(),
                    ],
                    Box::new(|word, _| {
                        let mode = word.parse::<FrameBufferColorMode>().ok()?;
                        Some(Command::NonTerminal(
                            ParamGreed::Rest,
                            vec![],
                            Box::new(move |rest, _| {
                                let args: Vec<&str> = rest.split_whitespace().collect();
                                match mode {
                                    FrameBufferColorMode::Grayscale => {
                                        if args.len() != 1 {
                                            return None;
                                        }
                                        let bits = args[0].parse::<u8>().ok()?;
                                        if !(1..=8).contains(&bits) {
                                            return None;
                                        }
                                        Some(Command::Terminal(Message::SetFrameBufferMode(
                                            mode, bits, 0, 0,
                                        )))
                                    }
                                    FrameBufferColorMode::Rgb | FrameBufferColorMode::YCbCr => {
                                        if args.len() != 3 {
                                            return None;
                                        }
                                        let bits1 = args[0].parse::<u8>().ok()?;
                                        let bits2 = args[1].parse::<u8>().ok()?;
                                        let bits3 = args[2].parse::<u8>().ok()?;
                                        if bits1 > 8 || bits2 > 8 || bits3 > 8 {
                                            return None;
                                        }
                                        Some(Command::Terminal(Message::SetFrameBufferMode(
                                            mode, bits1, bits2, bits3,
                                        )))
                                    }
                                }
                            }),
                        ))
                    }),
                )),
                "frame_buffer_set_width" => single_word(
                    vec![],
                    Box::new(|word| {
                        let width = word.parse::<usize>().ok()?.max(1);
                        Some(Command::Terminal(Message::SetFrameBufferWidth(width)))
                    }),
                ),
                "frame_buffer_set_range" => single_word(
                    vec![],
                    Box::new(|rest| {
                        let values: Vec<i64> = rest
                            .split_whitespace()
                            .map(str::parse::<i64>)
                            .collect::<Result<_, _>>()
                            .ok()?;
                        if values.is_empty() || !values.len().is_multiple_of(2) {
                            return None;
                        }

                        let pairs = values
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|c| (c[0], c[1]))
                            .collect::<Vec<_>>();
                        Some(Command::Terminal(Message::SetFrameBufferRange(pairs)))
                    }),
                ),
                "memory_viewer_open" => single_word(
                    arrays.clone(),
                    Box::new(|word| {
                        Some(Command::Terminal(Message::OpenMemoryViewer {
                            scope: ScopeRef::from_hierarchy_string(word),
                            name: Some(word.to_string()),
                        }))
                    }),
                ),
                "dump_tree" => Some(Command::Terminal(Message::DumpTree)),
                "group_add" => {
                    let option_values = option_values.clone();
                    Some(Command::NonTerminal(
                        ParamGreed::Custom(&optional_rest_split),
                        group_names
                            .iter()
                            .map(|name| quote_command_value(name))
                            .collect(),
                        Box::new(move |query, _| {
                            let (parts, options) = AddOptionSupport::Base
                                .parse_add_arguments(query, &option_values)?;
                            if parts.len() != 1 {
                                return None;
                            }
                            Some(Command::Terminal(Message::GroupAdd {
                                name: parts[0].clone(),
                                options,
                            }))
                        }),
                    ))
                }
                "group_marked" => optional_single_word(
                    vec![],
                    Box::new(|name| {
                        let trimmed = name.trim();
                        Some(Command::Terminal(Message::GroupNew {
                            name: (!trimmed.is_empty()).then_some(trimmed.to_owned()),
                            before: None,
                            items: None,
                        }))
                    }),
                ),
                "group_dissolve" => Some(Command::Terminal(Message::GroupDissolve(None))),
                "group_fold_recursive" => {
                    Some(Command::Terminal(Message::GroupFoldRecursive(None)))
                }
                "group_unfold_recursive" => {
                    Some(Command::Terminal(Message::GroupUnfoldRecursive(None)))
                }
                "group_fold_all" => Some(Command::Terminal(Message::GroupFoldAll)),
                "group_unfold_all" => Some(Command::Terminal(Message::GroupUnfoldAll)),
                "show_controls" => Some(Command::Terminal(Message::SetKeyHelpVisible(true))),
                "show_mouse_gestures" => {
                    Some(Command::Terminal(Message::SetGestureHelpVisible(true)))
                }
                "show_quick_start" => Some(Command::Terminal(Message::SetQuickStartVisible(true))),
                #[cfg(feature = "performance_plot")]
                "show_performance" => optional_single_word(
                    vec![],
                    Box::new(|word| {
                        if word == "redraw" {
                            Some(Command::Terminal(Message::Batch(vec![
                                Message::SetPerformanceVisible(true),
                                Message::SetContinuousRedraw(true),
                            ])))
                        } else {
                            Some(Command::Terminal(Message::SetPerformanceVisible(true)))
                        }
                    }),
                ),
                "cursor_set" => {
                    let timescale_for_cursor = timescale.clone();
                    single_word(
                        vec![],
                        Box::new(move |time_str| {
                            let time = if let Some(ts) = &timescale_for_cursor {
                                crate::time::parse_time_string_to_ticks(time_str, ts)?
                            } else {
                                time_str.parse().ok()?
                            };
                            Some(Command::Terminal(Message::Batch(vec![
                                Message::CursorSet(time),
                                Message::GoToCursorIfNotInView,
                            ])))
                        }),
                    )
                }
                "goto_time" => {
                    let timescale_for_goto = timescale.clone();
                    single_word(
                        vec![],
                        Box::new(move |time_str| {
                            let time = if let Some(ts) = &timescale_for_goto {
                                crate::time::parse_time_string_to_ticks(time_str, ts)?
                            } else {
                                time_str.parse().ok()?
                            };
                            Some(Command::Terminal(Message::GoToTime(Some(time), 0)))
                        }),
                    )
                }
                "marker_set" => {
                    let option_values = option_values.clone();
                    single_word(
                        marker_suggestions(&markers),
                        Box::new(move |query| {
                            let (parts, options) = AddOptionSupport::Base
                                .parse_add_arguments(query, &option_values)?;
                            if parts.len() != 1 {
                                return None;
                            }
                            let marker_ref = parts[0].clone();
                            let time = cursor.clone()?;
                            match parse_marker(&marker_ref, &markers) {
                                Some(id) => Some(Command::Terminal(Message::SetMarker {
                                    id,
                                    time,
                                    options: options.map(|o| o.as_item_options()),
                                })),
                                None => Some(Command::Terminal(Message::AddMarker {
                                    time,
                                    name: Some(marker_ref),
                                    move_focus: true,
                                    options: options.map(|o| o.as_item_options()),
                                })),
                            }
                        }),
                    )
                }
                "marker_set_at" => {
                    let timescale_for_marker_set_at = timescale.clone();
                    let option_values = option_values.clone();
                    Some(Command::NonTerminal(
                        ParamGreed::Rest,
                        vec![],
                        Box::new(move |query, _| {
                            let (parts, options) = AddOptionSupport::Base
                                .parse_add_arguments(query, &option_values)?;
                            if parts.len() < 2 {
                                return None;
                            }

                            let (time_str, marker_ref) = if parts.len() >= 3 {
                                let combined = format!("{}{}", parts[0], parts[1]);
                                if let Some(ts) = &timescale_for_marker_set_at {
                                    if crate::time::parse_time_string_to_ticks(&combined, ts)
                                        .is_some()
                                    {
                                        (combined, parts[2..].join(" "))
                                    } else {
                                        (parts[0].to_string(), parts[1..].join(" "))
                                    }
                                } else {
                                    (parts[0].to_string(), parts[1..].join(" "))
                                }
                            } else {
                                (parts[0].to_string(), parts[1].to_string())
                            };

                            let time = if let Some(ts) = &timescale_for_marker_set_at {
                                crate::time::parse_time_string_to_ticks(&time_str, ts)?
                            } else {
                                time_str.parse().ok()?
                            };

                            let marker_id = parse_marker(&marker_ref, &markers);
                            match marker_id {
                                Some(id) => Some(Command::Terminal(Message::SetMarker {
                                    id,
                                    time,
                                    options: options.map(|o| o.as_item_options()),
                                })),
                                None => Some(Command::Terminal(Message::AddMarker {
                                    time,
                                    name: Some(marker_ref),
                                    move_focus: true,
                                    options: options.map(|o| o.as_item_options()),
                                })),
                            }
                        }),
                    ))
                }
                "marker_remove" => Some(Command::NonTerminal(
                    ParamGreed::Rest,
                    marker_suggestions(&markers),
                    Box::new(move |name, _| {
                        let name = unquote_command_value(name.trim())?;
                        parse_marker(&name, &markers)
                            .map(|id| Command::Terminal(Message::RemoveMarker(id)))
                    }),
                )),
                "show_marker_window" => {
                    Some(Command::Terminal(Message::SetCursorWindowVisible(true)))
                }
                "show_logs" => Some(Command::Terminal(Message::SetLogsVisible(true))),
                "save_state" => Some(Command::Terminal(Message::SaveStateFile(
                    state_file.clone(),
                ))),
                "save_state_as" => single_word(
                    vec![],
                    Box::new(|word| {
                        Some(Command::Terminal(Message::SaveStateFile(Some(
                            Utf8PathBuf::from(word),
                        ))))
                    }),
                ),
                "load_state" => single_word(
                    vec![],
                    Box::new(|word| {
                        Some(Command::Terminal(Message::LoadStateFile(Some(
                            Utf8PathBuf::from(word),
                        ))))
                    }),
                ),
                "viewport_add" => Some(Command::Terminal(Message::AddViewport)),
                "viewport_remove" => Some(Command::Terminal(Message::RemoveViewport)),
                "viewport_set_active" => single_word(
                    viewport_indices.clone(),
                    Box::new(|word| {
                        let idx = word.parse::<usize>().ok()?;
                        Some(Command::Terminal(Message::SetActiveViewport(idx)))
                    }),
                ),
                "pause_simulation" => Some(Command::Terminal(Message::PauseSimulation)),
                "unpause_simulation" => Some(Command::Terminal(Message::UnpauseSimulation)),
                "undo" => Some(Command::Terminal(Message::Undo(1))),
                "redo" => Some(Command::Terminal(Message::Redo(1))),
                "wcp_server_start" => Some(Command::Terminal(Message::StartWcpServer {
                    address: None,
                    initiate: false,
                })),
                "wcp_server_stop" => Some(Command::Terminal(Message::StopWcpServer)),
                "exit" => Some(Command::Terminal(Message::Exit)),
                _ => None,
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format_names() -> Vec<String> {
        vec!["Hexadecimal".to_string(), "Clock".to_string()]
    }

    fn color_names() -> Vec<String> {
        vec!["red".to_string(), "black".to_string()]
    }

    fn make_option_values(format_names: Vec<String>, color_names: Vec<String>) -> AddOptionValues {
        AddOptionValues {
            format_names: Rc::new(format_names),
            color_names: Rc::new(color_names),
            group_names: Rc::new(Vec::new()),
            height_suggestions: Rc::new(Vec::new()),
            analog_suggestions: Rc::new(Vec::new()),
            y_axis_suggestions: Rc::new(Vec::new()),
        }
    }

    #[test]
    fn parse_add_arguments_accepts_options_in_any_order() {
        let option_values = make_option_values(format_names(), color_names());
        let result = AddOptionSupport::Waveform.parse_add_arguments(
            "tb.dut.counter --height 2 --background black -f Hexadecimal -c red",
            &option_values,
        );

        assert_eq!(
            result,
            Some((
                vec!["tb.dut.counter".to_string()],
                Some(AddVariableOptions {
                    format: Some("Hexadecimal".to_string()),
                    color: Some("red".to_string()),
                    background_color: Some("black".to_string()),
                    height: Some(2.0),
                    analog: None,
                    y_axis: None,
                    group: None,
                    position: None,
                    name: None,
                }),
            ))
        );
    }

    #[test]
    fn parse_add_arguments_rejects_invalid_options() {
        let format_names = format_names();
        let color_names = color_names();
        let option_values = make_option_values(format_names, color_names);
        let invalid_queries = [
            "signal --unknown value",
            "signal --color red --color black",
            "signal --format Hexadecimal --format Clock",
            "signal --format Hexadecimal --height nope",
            "signal --format Hexadecimal --background blue",
            "signal --analog off",
            "signal --position nope",
            "signal --position 1 --position 2",
        ];

        for query in invalid_queries {
            let support = if query.contains("--format") && query.contains("--background") {
                AddOptionSupport::Base
            } else {
                AddOptionSupport::Waveform
            };
            let result = support.parse_add_arguments(query, &option_values);
            assert!(result.is_none(), "query should be rejected: {query}");
        }
    }

    #[test]
    fn parse_add_arguments_accepts_group_and_position() {
        let option_values = make_option_values(format_names(), color_names());
        let (_, options) = AddOptionSupport::Waveform
            .parse_add_arguments("signal -g \"outer group\" -p 2", &option_values)
            .unwrap();

        let options = options.unwrap();
        assert_eq!(options.group, Some("outer group".to_string()));
        assert_eq!(options.position, Some(2));
    }

    #[test]
    fn parse_add_arguments_accepts_quoted_group_name() {
        let option_values = make_option_values(Vec::new(), color_names());
        let (parts, options) = AddOptionSupport::Base
            .parse_add_arguments("\"Group name\" --color red", &option_values)
            .unwrap();

        assert_eq!(parts, vec!["Group name"]);
        assert_eq!(options.unwrap().color, Some("red".to_string()));
    }

    #[test]
    fn quoted_add_option_values_preserve_spaces() {
        let format_names = vec!["Wide Bus".to_string()];
        let color_names = color_names();
        let option_values = make_option_values(format_names, color_names);

        assert_eq!(
            split_command_words("signal --format \"Wide Bus\"").unwrap(),
            vec!["signal", "--format", "Wide Bus"]
        );
        let (parts, options) = AddOptionSupport::Waveform
            .parse_add_arguments("signal --format \"Wide Bus\"", &option_values)
            .unwrap();
        assert_eq!(parts, vec!["signal"]);
        assert_eq!(options.unwrap().format, Some("Wide Bus".to_string()));
    }

    #[test]
    fn quoted_divider_name_is_parsed_from_the_complete_tail() {
        let query = "\"Clock and reset\" --color Green --background Gray";
        let (_, current, delimiter, rest) = optional_rest_split(query);

        assert_eq!(current, query);
        assert!(delimiter.is_empty());
        assert!(rest.is_empty());

        let (parts, options) = AddOptionSupport::Base
            .parse_add_arguments(
                &current,
                &make_option_values(Vec::new(), vec!["Green".to_string(), "Gray".to_string()]),
            )
            .unwrap();

        assert_eq!(parts, vec!["Clock and reset"]);
        let options = options.unwrap();
        assert_eq!(options.color, Some("Green".to_string()));
        assert_eq!(options.background_color, Some("Gray".to_string()));
    }

    #[test]
    fn option_value_suggestions_quote_spaces() {
        assert_eq!(quote_command_value("Wide Bus"), "\"Wide Bus\"");
        assert_eq!(quote_command_value("Hexadecimal"), "Hexadecimal");
        assert_eq!(quote_command_value("type limits"), "\"type limits\"");
        assert_eq!(
            unquote_command_value("\"Wide Bus\""),
            Some("Wide Bus".to_string())
        );
    }

    #[test]
    fn parse_add_arguments_accepts_lowercase_y_axis_values() {
        let option_values = make_option_values(format_names(), color_names());
        let (_, options) = AddOptionSupport::Waveform
            .parse_add_arguments(
                "signal --yaxis \"type limits\" --analog interpolated",
                &option_values,
            )
            .unwrap();

        assert_eq!(
            options.unwrap().analog,
            Some(AnalogRenderStyle::Interpolated)
        );
    }

    #[test]
    fn container_type_selects_expected_option_support() {
        assert!(matches!(
            AddOptionSupport::from_container_type(false),
            AddOptionSupport::Waveform
        ));
        assert!(matches!(
            AddOptionSupport::from_container_type(true),
            AddOptionSupport::Base
        ));
    }

    #[test]
    fn option_support_restricts_waveform_options_to_waveforms() {
        let waveform_options = [
            AddOption::Format,
            AddOption::Height,
            AddOption::Analog,
            AddOption::YAxis,
        ];

        for option in waveform_options {
            assert!(AddOptionSupport::Waveform.supports(option));
            assert!(!AddOptionSupport::Base.supports(option));
        }

        assert!(AddOptionSupport::Base.supports(AddOption::Color));
        assert!(AddOptionSupport::Base.supports(AddOption::Background));
        assert!(AddOptionSupport::Base.supports(AddOption::Group));
        assert!(AddOptionSupport::Base.supports(AddOption::Position));
    }
}
