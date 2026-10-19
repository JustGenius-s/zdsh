mod command_builder;
pub mod fig_types;
mod signature;

use serde::{Deserialize, Serialize};
pub use signature::*;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

pub use command_builder::CommandBuilder;

const MIN_ORDER_VAL: u32 = 1;
const MAX_ORDER_VAL: u32 = 100;

#[derive(Clone, Copy, Debug, Ord, PartialOrd, Eq, PartialEq, Serialize, Deserialize)]
pub enum IconType {
    File,
    Folder,
    GitBranch,
    KubePod,
    KubeCluster,
    DockerContainer,
    DockerImage,
}

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    /// The exact string to be inserted, should the suggestion be accepted. Maps to Fig's `name`
    /// field.
    pub exact_string: String,
    /// If provided, is used as the display value for the suggestion in the menu. Maps to Fig's
    /// `displayValue` field.
    pub display_name: Option<String>,
    /// Helper text to describe what kind of suggestion this is. Maps to Fig's `description` field.
    /// e.g. "Container" for a Docker container suggestion vs. a Docker image suggestion.
    pub description: Option<String>,
    pub priority: Priority,
    /// We have default flags based on type of suggestion (command, flag, argument, etc).
    /// This provides a way for generators to override the default one with a different icon.
    pub icon: Option<IconType>,
    /// If a suggestion is hidden, we only show it if what the user has typed matches exactly with the suggestion string.
    pub is_hidden: bool,
}

impl Suggestion {
    pub fn new(name: impl Into<String>) -> Self {
        Suggestion {
            exact_string: name.into(),
            display_name: None,
            description: None,
            priority: Priority::Default,
            icon: None,
            is_hidden: false,
        }
    }

    pub fn with_description(name: impl Into<String>, description: impl Into<String>) -> Self {
        Suggestion {
            exact_string: name.into(),
            display_name: None,
            description: Some(description.into()),
            priority: Priority::Default,
            icon: None,
            is_hidden: false,
        }
    }

    pub fn with_display_name(mut self, display_name: Option<String>) -> Self {
        self.display_name = display_name;
        self
    }

    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_icon(mut self, icon: IconType) -> Self {
        self.icon = Some(icon);
        self
    }
}

#[allow(clippy::derived_hash_with_manual_eq)]
impl Hash for Suggestion {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.exact_string.hash(state)
    }
}

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq, Serialize)]
pub enum PathSuggestionType {
    File,
    Folder,
}

impl PathSuggestionType {
    pub fn is_folder(&self) -> bool {
        matches!(self, PathSuggestionType::Folder)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Priority {
    /// Ordering for suggestions that can be ordered above or below all of the other suggestions
    /// (e.g. the current branch should be the first suggestion that shows up)
    Global(Importance),
    /// Ordering for suggestions that can be ordered above or below all of the other suggestions in the same group
    /// (e.g. the --help option should be the first option that appears among all other options)
    Local(Importance),

    /// No special priority
    #[default]
    Default,
}

impl Priority {
    pub fn is_global(&self) -> bool {
        matches!(self, Priority::Global(_))
    }

    pub fn most_important() -> Self {
        Priority::Global(Importance::More(Order(MAX_ORDER_VAL)))
    }
}

impl Ord for Priority {
    fn cmp(&self, other: &Self) -> Ordering {
        use Importance::*;
        use Priority::*;

        match (self, other) {
            // If we're comparing two Globals or two Locals, compare their importances.
            (Global(self_relativity), Global(other_relativity))
            | (Local(self_relativity), Local(other_relativity)) => {
                self_relativity.cmp(other_relativity)
            }

            // Two defaults are always equal
            (Default, Default) => Ordering::Equal,

            // If we're comparing a Global::More, it will always be greater than any other Local/Default.
            // Similarly, a Global::Less will always be less than any other Local/Default.
            (Global(More(_)), _) | (_, Global(Less(_))) => Ordering::Greater,
            (_, Global(More(_))) | (Global(Less(_)), _) => Ordering::Less,

            // Finally, we compare Local's with Default. A Local::More will always be greater than a Default.
            // Similarly, a Local::Less will always be less than a Default.
            (Local(More(_)), Default) | (Default, Local(Less(_))) => Ordering::Greater,
            (Default, Local(More(_))) | (Local(Less(_)), Default) => Ordering::Less,
        }
    }
}

impl PartialOrd for Priority {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Importance {
    /// More is reserved for suggestions that should be ordered with more priority relative to another suggestion.
    /// The higher the order, the more priority it should have.
    More(Order),
    /// Less is reserved for suggestions that should be ordered with less priority relative to another suggestion.
    /// The higher the order, the more priority it should have.
    Less(Order),
}

impl Ord for Importance {
    fn cmp(&self, other: &Self) -> Ordering {
        use Importance::*;
        match (self, other) {
            (More(self_weight), More(other_weight)) | (Less(self_weight), Less(other_weight)) => {
                self_weight.normalized().cmp(&other_weight.normalized())
            }
            (More(_), Less(_)) => Ordering::Greater,
            (Less(_), More(_)) => Ordering::Less,
        }
    }
}

impl PartialOrd for Importance {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Order(pub u32);
impl Order {
    fn normalized(self) -> Self {
        let bounded_weight: u32 = self.0.clamp(MIN_ORDER_VAL, MAX_ORDER_VAL);
        Order(bounded_weight)
    }
}

pub type Aliases = HashMap<AliasGeneratorName, Alias>;
pub type Generators = HashMap<GeneratorName, Generator>;

#[derive(Clone)]
pub struct CommandSignatureGenerators {
    command_name: String,
    generators: Generators,
    filters: Filters,
    aliases: Aliases,
}

/// Struct containing metadata needed to produce dynamic completion results.
pub struct DynamicCompletionData {
    generators: Generators,
    filters: Filters,
    aliases: Aliases,
}

impl DynamicCompletionData {
    pub fn generators(&self) -> &Generators {
        &self.generators
    }

    pub fn filters(&self) -> &Filters {
        &self.filters
    }

    pub fn aliases(&self) -> &Aliases {
        &self.aliases
    }
}

impl From<CommandSignatureGenerators> for (String, DynamicCompletionData) {
    fn from(command_generators: CommandSignatureGenerators) -> Self {
        (
            command_generators.command_name,
            DynamicCompletionData {
                generators: command_generators.generators,
                filters: command_generators.filters,
                aliases: command_generators.aliases,
            },
        )
    }
}

/// The shell type that a command will be run in.
#[derive(Copy, Clone)]
pub enum Shell {
    /// A shell that can assume POSIX-compliant syntax.
    Posix,
    Powershell,
    CmdExe,
}

impl Shell {
    /// Returns the shell-specific way to ignore `stderr`.
    fn ignore_stderr(&self) -> &'static str {
        match self {
            Shell::Posix => "2>/dev/null",
            Shell::Powershell => "2> $null",
            Shell::CmdExe => "2> NUL",
        }
    }
}

impl CommandSignatureGenerators {
    pub fn new(command_name: impl AsRef<str>) -> Self {
        Self {
            command_name: command_name.as_ref().to_lowercase(),
            generators: HashMap::new(),
            filters: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    pub fn add_generator(
        mut self,
        generator_name: impl Into<GeneratorName>,
        generator: Generator,
    ) -> Self {
        self.generators.insert(generator_name.into(), generator);
        self
    }

    pub fn add_filter(
        mut self,
        filter_name: impl Into<FilterTemplateSuggestion>,
        filter: TemplateFilter,
    ) -> Self {
        self.filters.insert(filter_name.into(), filter);
        self
    }

    pub fn add_alias(mut self, alias_name: impl Into<AliasGeneratorName>, alias: Alias) -> Self {
        self.aliases.insert(alias_name.into(), alias);
        self
    }

    pub fn generators(&self) -> &Generators {
        &self.generators
    }
}

pub type Filters = HashMap<FilterTemplateSuggestion, TemplateFilter>;

#[derive(Clone)]
pub struct TemplateFilters {
    command_name: String,
    filters: Filters,
}

impl From<TemplateFilters> for (String, Filters) {
    fn from(command_generators: TemplateFilters) -> Self {
        (command_generators.command_name, command_generators.filters)
    }
}

impl TemplateFilters {
    pub fn new(command_name: impl Into<String>) -> Self {
        Self {
            command_name: command_name.into(),
            filters: HashMap::new(),
        }
    }

    pub fn add_filter(
        mut self,
        filter_name: impl Into<FilterTemplateSuggestion>,
        filter: TemplateFilter,
    ) -> Self {
        self.filters.insert(filter_name.into(), filter);
        self
    }

    pub fn filters(&self) -> &Filters {
        &self.filters
    }
}
