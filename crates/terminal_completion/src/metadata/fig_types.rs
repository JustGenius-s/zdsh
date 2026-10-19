use crate::metadata::{
    AliasGeneratorName, Argument, ArgumentType, FilterTemplateSuggestion, GeneratorName,
    Importance, IsArgumentOptional, Opt, Order, Priority, Signature,
};
use serde::{Deserialize, Serialize};
use serde_with::formats::{PreferMany, PreferOne};
use serde_with::{NoneAsEmptyString, OneOrMany, serde_as};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::convert::TryFrom;
use std::fmt;
use std::fmt::{Display, Formatter};
use std::ops::Not;

use itertools::Itertools;

/// https://fig.io/docs/reference/suggestion/indicating-priority
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FigPriority(pub u32);

/// Mapping to the exact types of Fig's completion specs at commit
/// 67b2515c5ea4ff4c70c672a5ccdf8a77547d1366.
/// See https://github.com/withfig/autocomplete-tools/blob/67b2515c5ea4ff4c70c672a5ccdf8a77547d1366/packages/autocomplete-types/index.d.ts
/// for the original type definition.
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
pub enum SuggestionType {
    #[serde(rename = "folder")]
    Folder,
    #[serde(rename = "file")]
    File,
    #[serde(rename = "arg")]
    #[serde(alias = "argument")]
    Arg,
    #[serde(rename = "subcommand")]
    Subcommand,
    #[serde(rename = "option")]
    Option,
    #[serde(rename = "special")]
    Special,
    #[serde(rename = "shortcut")]
    Shortcut,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Clone)]
pub struct Suggestion {
    #[serde(default)]
    #[serde_as(as = "OneOrMany<_, PreferOne>")]
    pub name: Vec<String>,

    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,

    #[serde(default, rename = "type")]
    pub suggestion_type: Option<SuggestionType>,

    #[serde(default)]
    pub description: Option<String>,

    #[serde(default, rename = "isDangerous", skip_serializing_if = "<&bool>::not")]
    pub is_dangerous: bool,

    #[serde(default)]
    pub priority: Option<FigPriority>,

    #[serde(default, skip_serializing_if = "<&bool>::not")]
    pub hidden: bool,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Deserialize, Serialize, Debug, PartialEq)]
pub struct Command {
    #[serde_as(as = "OneOrMany<_, PreferOne>")]
    pub name: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subcommands: Vec<Command>,

    #[serde(default)]
    pub options: Vec<CommandOption>,

    #[serde(default)]
    #[serde_as(deserialize_as = "OneOrMany<_, PreferMany>")]
    pub args: Vec<Arg>,

    #[serde(default, rename = "aliasGenerator")]
    pub alias_generator: Option<AliasGeneratorName>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_suggestions: Vec<Suggestion>,

    #[serde(default)]
    #[serialize_always]
    #[serde_as(as = "NoneAsEmptyString")]
    pub description: Option<String>,

    #[serde(default, rename = "isDangerous", skip_serializing_if = "<&bool>::not")]
    pub is_dangerous: bool,

    #[serde(default)]
    pub priority: Option<FigPriority>,

    #[serde(default, skip_serializing_if = "<&bool>::not")]
    pub hidden: bool,

    #[serde(
        default,
        rename = "parserDirectives",
        skip_serializing_if = "ParserDirectives::is_default"
    )]
    pub parser_directives: ParserDirectives,
}

/// Configure how the completion engine will map raw tokens to options/flags in the spec.
/// Basically back-porting this concept from Fig:
/// https://fig.io/docs/reference/arg#parserdirectives
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct ParserDirectives {
    /// Flags with long names may begin with just 1 hyphen instead of 2.
    /// https://www.gnu.org/software/libc/manual/html_node/Argument-Syntax.html
    #[serde(
        default,
        rename = "flagsArePosixNoncompliant",
        skip_serializing_if = "<&bool>::not"
    )]
    pub flags_are_posix_noncompliant: bool,

    /// Flags don't need to be spelled out in full, e.g. for `Get-ChildItem` you can provide "-Fi"
    /// instead of "-Filter", but not just "-F" as it might match "-Filter" or "-Force".
    #[serde(
        default,
        rename = "flagsMatchUniquePrefix",
        skip_serializing_if = "<&bool>::not"
    )]
    pub flags_match_unique_prefix: bool,

    /// This command is case-insensitive _even if_ the user's filesystem is case-sensitive.
    #[serde(
        default,
        rename = "alwaysCaseInsensitive",
        skip_serializing_if = "<&bool>::not"
    )]
    pub always_case_insensitive: bool,
}

impl ParserDirectives {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(untagged)]
pub enum NumberOrBool {
    Number(usize),
    Bool(bool),
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Default, Debug, PartialEq, Clone)]
pub struct CommandOption {
    #[serde(default)]
    #[serde_as(as = "OneOrMany<_, PreferOne>")]
    pub name: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(deserialize_as = "OneOrMany<_, PreferMany>")]
    pub args: Vec<Arg>,

    #[serde(default, rename = "isPersistent", skip_serializing_if = "<&bool>::not")]
    pub is_persistent: bool,

    #[serde(default, rename = "isRequired", skip_serializing_if = "<&bool>::not")]
    pub is_required: bool,

    #[serde(
        default,
        rename = "requiresEquals",
        skip_serializing_if = "<&bool>::not"
    )]
    pub requires_equals: bool,

    // TODO: we should be using this option to determine if an option can be repeated.
    #[serde(default, rename = "isRepeatable")]
    pub is_repeatable: Option<NumberOrBool>,

    #[serde(default, rename = "exclusiveOn", skip_serializing_if = "Vec::is_empty")]
    pub exclusive_on: Vec<String>,

    #[serde(default, rename = "dependsOn", skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    #[serde(default)]
    pub description: Option<String>,

    #[serde(default, rename = "isDangerous", skip_serializing_if = "<&bool>::not")]
    pub is_dangerous: bool,

    #[serde(default)]
    pub priority: Option<FigPriority>,

    #[serde(default, skip_serializing_if = "<&bool>::not")]
    pub hidden: bool,
}

fn default_skip_generator_validation() -> bool {
    true
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Clone)]
pub struct Arg {
    #[serde(default)]
    #[serialize_always]
    #[serde_as(as = "NoneAsEmptyString")]
    pub name: Option<String>,

    #[serde(default)]
    pub description: Option<String>,

    #[serde(default, rename = "isDangerous", skip_serializing_if = "<&bool>::not")]
    pub is_dangerous: bool,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<NameOrSuggestion>,

    #[serde(
        default,
        rename = "generatorName",
        skip_serializing_if = "Vec::is_empty"
    )]
    #[serde_as(as = "OneOrMany<_, PreferOne>")]
    pub generator_name: Vec<GeneratorName>,

    #[serde(
        default = "default_skip_generator_validation",
        rename = "skipGeneratorValidation",
        skip_serializing_if = "<&bool>::not"
    )]
    pub skip_generator_validation: bool,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde_as(as = "OneOrMany<_, PreferOne>")]
    pub template: Vec<Template>,

    #[serde(default, rename = "filterTemplateName")]
    pub filter_template_suggestions: Option<FilterTemplateSuggestion>,

    #[serde(default, rename = "isVariadic", skip_serializing_if = "<&bool>::not")]
    pub is_variadic: bool,

    #[serde(default, rename = "isOptional", skip_serializing_if = "<&bool>::not")]
    pub is_optional: bool,

    #[serde(default, rename = "isCommand", skip_serializing_if = "<&bool>::not")]
    pub is_command: bool,

    /// The default value for an optional argument. This is just a string.
    #[serde(default)]
    pub default: Option<StringOrNumber>,
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(untagged)]
pub enum StringOrNumber {
    String(String),
    Number(usize),
}

impl From<StringOrNumber> for String {
    fn from(string_or_number: StringOrNumber) -> Self {
        match string_or_number {
            StringOrNumber::String(s) => s,
            StringOrNumber::Number(number) => number.to_string(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
pub enum Template {
    #[serde(rename = "filepaths")]
    FilePaths,

    #[serde(rename = "filepathsMayNotExist")]
    FilePathsMayNotExist,

    #[serde(rename = "folders")]
    Folders,

    #[serde(rename = "foldersMayNotExist")]
    FoldersMayNotExist,

    #[serde(rename = "history")]
    History,

    #[serde(rename = "help")]
    Help,
}

impl From<String> for Suggestion {
    fn from(name: String) -> Self {
        Suggestion {
            name: vec![name],
            ..Default::default()
        }
    }
}

impl Display for Suggestion {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.name,)
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
#[serde(untagged)]
pub enum NameOrSuggestion {
    Name(String),
    Suggestion(Suggestion),
}

impl From<NameOrSuggestion> for Vec<crate::metadata::Suggestion> {
    fn from(name_or_suggestion: NameOrSuggestion) -> Self {
        match name_or_suggestion {
            NameOrSuggestion::Name(name) => {
                vec![crate::metadata::Suggestion::new(html_unescape(name))]
            }
            NameOrSuggestion::Suggestion(suggestion) => suggestion.into(),
        }
    }
}

impl From<Suggestion> for NameOrSuggestion {
    fn from(suggestion: Suggestion) -> Self {
        NameOrSuggestion::Suggestion(suggestion)
    }
}

impl From<Command> for Vec<Signature> {
    fn from(command: Command) -> Self {
        let persistent_options = command
            .options
            .iter()
            .filter(|&option| option.is_persistent)
            .cloned()
            .collect_vec();

        let arguments = if command.args.is_empty() {
            None
        } else {
            Some(command.args.into_iter().map(Into::into).collect_vec())
        };

        let subcommands = if command.subcommands.is_empty() {
            None
        } else {
            Some(
                command
                    .subcommands
                    .into_iter()
                    .flat_map(|mut subcommand| {
                        subcommand.options.extend(persistent_options.clone());
                        Vec::from(subcommand)
                    })
                    .collect_vec(),
            )
        };

        let options = if command.options.is_empty() {
            None
        } else {
            Some(command.options.into_iter().map(Into::into).collect_vec())
        };

        let description_unescaped = command.description.map(html_unescape);

        command
            .name
            .into_iter()
            .map(|name| Signature {
                name: html_unescape(name),
                alias_generator: command.alias_generator.clone(),
                description: description_unescaped.clone(),
                arguments: arguments.clone(),
                subcommands: subcommands.clone(),
                options: options.clone(),
                priority: command.priority.map_or_else(Priority::default, Into::into),
                parser_directives: command.parser_directives.clone().into(),
            })
            .collect()
    }
}

impl From<Arg> for Argument {
    fn from(arg: Arg) -> Self {
        // The order of argument_types will dictate the order in which completions will be surfaced.
        // Currently, the order is Generators followed by Suggestions followed by Templates.
        // - If there are multiple static Suggestions, they will be ordered lexicographically
        //   (but `priority` can be used to reorder).
        // - If there are multiple generators/templates for a given Argument, the order in which they
        //   are listed in the spec is the order in which they will be grouped.
        //   For example, if argument_types = vec![generator_a, generator_b], then generator_a
        //   completions will come before generator_b completions.
        let argument_types = arg
            .generator_name
            .into_iter()
            .map(ArgumentType::Generator)
            .chain(
                arg.suggestions
                    .into_iter()
                    .flat_map(Vec::from)
                    .sorted()
                    .map(ArgumentType::Suggestion),
            )
            .chain(arg.template.into_iter().filter_map(|template| {
                crate::metadata::TemplateType::try_from(template)
                    .ok()
                    .map(|template_type| {
                        ArgumentType::Template(crate::metadata::Template {
                            type_name: template_type,
                            filter_name: arg.filter_template_suggestions.clone(),
                        })
                    })
            }))
            .collect();

        let optional = if arg.is_optional {
            IsArgumentOptional::Optional(arg.default.map(Into::into))
        } else {
            IsArgumentOptional::Required
        };

        let display_name = arg.name.map(html_unescape);
        let description = arg.description.map(html_unescape);

        Argument {
            display_name,
            description,
            is_variadic: arg.is_variadic,
            argument_types,
            optional,
            is_command: arg.is_command,
            skip_generator_validation: arg.skip_generator_validation,
        }
    }
}

/// https://fig.io/docs/reference/suggestion/indicating-priority
/// 50 is default, so < 50 is Lower and > 50 is Higher
impl From<FigPriority> for Priority {
    fn from(priority: FigPriority) -> Self {
        let order = Order(priority.0).normalized();
        let default_order = Order(50);
        match order.cmp(&default_order) {
            Ordering::Less => Priority::Global(Importance::Less(order)),
            Ordering::Greater => Priority::Global(Importance::More(order)),
            Ordering::Equal => Priority::Default,
        }
    }
}

impl From<Suggestion> for Vec<crate::metadata::Suggestion> {
    fn from(suggestion: Suggestion) -> Self {
        let display_name = suggestion.display_name.map(html_unescape);
        let description = suggestion.description.map(html_unescape);
        suggestion
            .name
            .into_iter()
            .map(|name| crate::metadata::Suggestion {
                exact_string: html_unescape(name),
                display_name: display_name.clone(),
                description: description.clone(),
                priority: suggestion
                    .priority
                    .map_or_else(Priority::default, Into::into),
                icon: None,
                is_hidden: suggestion.hidden,
            })
            .collect()
    }
}

impl From<CommandOption> for Opt {
    fn from(option: CommandOption) -> Self {
        Opt {
            exact_string: option.name.into_iter().map(html_unescape).collect(),
            description: option.description.map(html_unescape),
            arguments: if option.args.is_empty() {
                None
            } else {
                Some(option.args.into_iter().map(|a| a.into()).collect())
            },
            required: option.is_required,
            priority: option.priority.map_or_else(Priority::default, Into::into),
        }
    }
}

impl TryFrom<Template> for crate::metadata::TemplateType {
    type Error = ();

    fn try_from(template: Template) -> Result<Self, Self::Error> {
        match template {
            Template::FilePaths => Ok(crate::metadata::TemplateType::Files { must_exist: true }),
            Template::FilePathsMayNotExist => {
                Ok(crate::metadata::TemplateType::Files { must_exist: false })
            }
            Template::Folders => Ok(crate::metadata::TemplateType::Folders { must_exist: true }),
            Template::FoldersMayNotExist => {
                Ok(crate::metadata::TemplateType::Folders { must_exist: false })
            }
            Template::History | Template::Help => Err(()),
        }
    }
}

impl From<ParserDirectives> for crate::metadata::ParserDirectives {
    fn from(value: ParserDirectives) -> Self {
        let ParserDirectives {
            flags_are_posix_noncompliant,
            flags_match_unique_prefix,
            always_case_insensitive,
        } = value;
        Self {
            flags_are_posix_noncompliant,
            flags_match_unique_prefix,
            always_case_insensitive,
        }
    }
}

/// Unescape HTML entities that Fig allows in `name` or `description` fields.
fn html_unescape(input: String) -> String {
    match html_escape::decode_html_entities(&input) {
        // If the input is returned as-is, it contains no HTML entities so we can reuse the owned String.
        Cow::Borrowed(_) => input,
        Cow::Owned(transformed) => transformed,
    }
}
