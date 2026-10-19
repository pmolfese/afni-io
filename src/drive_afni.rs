//! Lossless parsing and description of AFNI's `DRIVE_AFNI` command language.
//!
//! This module models command text; it does not execute commands or depend on
//! viewer state. The static dispatch table follows `afni_driver.c` in source
//! order because AFNI performs a case-sensitive **prefix** comparison and the
//! order therefore affects ambiguous names. [`DispatchMode::Exact`] is
//! available to receivers that prefer an exact command token.
//!
//! Unknown commands are preserved by default. AFNI plugins can register new
//! command labels at runtime, so rejecting every command absent from the
//! static table would break legitimate legacy scripts. A receiver can use
//! [`Registry`] for commands it knows were registered or select
//! [`UnknownPolicy::Reject`] when it needs a closed vocabulary.

use std::fmt;

/// Half-open byte range into the original command text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// First byte included in the span.
    pub start: usize,
    /// First byte after the span.
    pub end: usize,
}

impl Span {
    /// Construct a half-open span.
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Number of bytes covered by the span.
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    /// Whether the span is empty.
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Slice the source text at this span.
    pub fn slice(self, source: &str) -> &str {
        &source[self.start..self.end]
    }
}

/// Observable effects a command may have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Effects(u16);

impl Effects {
    /// No externally visible effect.
    pub const NONE: Self = Self(0);
    /// Returns or prints information.
    pub const QUERY: Self = Self(1 << 0);
    /// Mutates viewer/application state.
    pub const STATE_MUTATION: Self = Self(1 << 1);
    /// Reads a file or directory supplied by the command.
    pub const FILE_READ: Self = Self(1 << 2);
    /// Writes or appends to a file supplied by the command.
    pub const FILE_WRITE: Self = Self(1 << 3);
    /// Changes process lifecycle or deliberately blocks the UI process.
    pub const PROCESS_LIFECYCLE: Self = Self(1 << 4);
    /// Mutates the process environment or working directory.
    pub const ENVIRONMENT_MUTATION: Self = Self(1 << 5);
    /// Executes arbitrary shell text.
    pub const ARBITRARY_SHELL: Self = Self(1 << 6);

    /// Combine two effect sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether all effects in `other` are present.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Raw stable bit representation for serialization or policy tables.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Conservative risk tier implied by these effects.
    pub fn risk(self) -> Risk {
        if self.contains(Self::ARBITRARY_SHELL) {
            Risk::ArbitraryCode
        } else if self.contains(Self::PROCESS_LIFECYCLE) {
            Risk::ProcessControl
        } else if self.contains(Self::ENVIRONMENT_MUTATION) {
            Risk::Environment
        } else if self.contains(Self::FILE_WRITE) {
            Risk::FileWrite
        } else if self.contains(Self::FILE_READ) {
            Risk::FileRead
        } else if self.contains(Self::STATE_MUTATION) {
            Risk::StateMutation
        } else {
            Risk::Observe
        }
    }
}

/// Conservative command risk tier, ordered from observation to arbitrary code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Risk {
    /// No known mutation; queries may still disclose viewer state.
    Observe,
    /// Changes application/viewer state.
    StateMutation,
    /// Reads caller-selected files.
    FileRead,
    /// Writes caller-selected files.
    FileWrite,
    /// Changes process environment or working directory.
    Environment,
    /// Quits or deliberately blocks the target process.
    ProcessControl,
    /// Executes an arbitrary shell command.
    ArbitraryCode,
    /// Effects of an unregistered raw command are unknown.
    Unknown,
}

/// Broad argument form used for discovery and UI generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArgumentShape {
    /// No documented arguments.
    None,
    /// Whitespace/quote-aware argument tokens.
    Tokens,
    /// The remainder is principally free-form text.
    Remainder,
    /// First semantic argument names an AFNI viewer window.
    Window,
}

/// One entry in AFNI's compiled-in dispatch table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommandSpec {
    /// Case-sensitive label accepted by AFNI.
    pub name: &'static str,
    /// Preferred spelling shared by aliases.
    pub canonical: &'static str,
    /// Whether the command accepts an optional `A`–`J` controller prefix.
    pub controller: bool,
    /// Broad argument form.
    pub arguments: ArgumentShape,
    /// Potential observable effects.
    pub effects: Effects,
}

impl CommandSpec {
    /// Whether this label is an alias of another static label.
    pub const fn is_alias(self) -> bool {
        !same_str(self.name, self.canonical)
    }
}

const fn same_str(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

const STATE: Effects = Effects::STATE_MUTATION;
const QUERY: Effects = Effects::QUERY;
const READ_STATE: Effects = Effects::FILE_READ.union(Effects::STATE_MUTATION);
const WRITE: Effects = Effects::FILE_WRITE;
const WRITE_STATE: Effects = Effects::FILE_WRITE.union(Effects::STATE_MUTATION);
const PROCESS: Effects = Effects::PROCESS_LIFECYCLE;
const ENVIRONMENT: Effects = Effects::ENVIRONMENT_MUTATION;
const SHELL: Effects = Effects::ARBITRARY_SHELL
    .union(Effects::PROCESS_LIFECYCLE)
    .union(Effects::FILE_READ)
    .union(Effects::FILE_WRITE);

const fn spec(
    name: &'static str,
    canonical: &'static str,
    controller: bool,
    arguments: ArgumentShape,
    effects: Effects,
) -> CommandSpec {
    CommandSpec {
        name,
        canonical,
        controller,
        arguments,
        effects,
    }
}

/// AFNI's compiled-in `dpair` table in exact dispatch order.
///
/// The duplicate `WRITE_UNDERLAY` entry is retained because it is present in
/// the C table. Use [`static_command`] for a unique lookup.
pub static STATIC_COMMANDS: &[CommandSpec] = &[
    spec(
        "RESCAN_THIS",
        "RESCAN_THIS",
        true,
        ArgumentShape::Tokens,
        READ_STATE,
    ),
    spec(
        "SET_SESSION",
        "SET_SESSION",
        true,
        ArgumentShape::Remainder,
        READ_STATE,
    ),
    spec(
        "SWITCH_SESSION",
        "SET_SESSION",
        true,
        ArgumentShape::Remainder,
        READ_STATE,
    ),
    spec(
        "SWITCH_DIRECTORY",
        "SET_SESSION",
        true,
        ArgumentShape::Remainder,
        READ_STATE,
    ),
    spec(
        "SET_ANATOMY",
        "SET_ANATOMY",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SWITCH_ANATOMY",
        "SET_ANATOMY",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SWITCH_UNDERLAY",
        "SET_ANATOMY",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_UNDERLAY",
        "SET_ANATOMY",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_FUNCTION",
        "SET_FUNCTION",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SWITCH_FUNCTION",
        "SET_FUNCTION",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SWITCH_OVERLAY",
        "SET_FUNCTION",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_OVERLAY",
        "SET_FUNCTION",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_SUBBRICKS",
        "SET_SUBBRICKS",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_SUB_BRICKS",
        "SET_SUBBRICKS",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "PURGE_MEMORY",
        "PURGE_MEMORY",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "OPEN_WINDOW",
        "OPEN_WINDOW",
        true,
        ArgumentShape::Window,
        STATE,
    ),
    spec(
        "ALTER_WINDOW",
        "OPEN_WINDOW",
        true,
        ArgumentShape::Window,
        STATE,
    ),
    spec(
        "SET_WINDOW",
        "OPEN_WINDOW",
        true,
        ArgumentShape::Window,
        STATE,
    ),
    spec(
        "CLOSE_WINDOW",
        "CLOSE_WINDOW",
        true,
        ArgumentShape::Window,
        STATE,
    ),
    spec("SAVE_JPEG", "SAVE_JPEG", true, ArgumentShape::Window, WRITE),
    spec("SAVE_PNG", "SAVE_PNG", true, ArgumentShape::Window, WRITE),
    spec(
        "SAVE_RAWMONT",
        "SAVE_RAWMONT",
        true,
        ArgumentShape::Window,
        WRITE,
    ),
    spec("SAVE_RAW", "SAVE_RAW", true, ArgumentShape::Window, WRITE),
    spec("SAVE_MPEG", "SAVE_MPEG", true, ArgumentShape::Window, WRITE),
    spec("SAVE_AGIF", "SAVE_AGIF", true, ArgumentShape::Window, WRITE),
    spec(
        "SAVE_ALLJPEG",
        "SAVE_ALLJPEG",
        true,
        ArgumentShape::Window,
        WRITE,
    ),
    spec(
        "SAVE_ALLPNG",
        "SAVE_ALLPNG",
        true,
        ArgumentShape::Window,
        WRITE,
    ),
    spec(
        "SAVE_FILTERED",
        "SAVE_FILTERED",
        true,
        ArgumentShape::Window,
        SHELL,
    ),
    spec("SET_VIEW", "SET_VIEW", true, ArgumentShape::Tokens, STATE),
    spec(
        "SET_DICOM_XYZ",
        "SET_DICOM_XYZ",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "GET_DICOM_XYZ",
        "GET_DICOM_XYZ",
        true,
        ArgumentShape::Tokens,
        QUERY,
    ),
    spec(
        "GET_OLAY_VAL",
        "GET_OLAY_VAL",
        true,
        ArgumentShape::Tokens,
        QUERY,
    ),
    spec(
        "GET_ULAY_VAL",
        "GET_ULAY_VAL",
        true,
        ArgumentShape::Tokens,
        QUERY,
    ),
    spec(
        "GET_THR_VAL",
        "GET_THR_VAL",
        true,
        ArgumentShape::Tokens,
        QUERY,
    ),
    spec(
        "GET_ULAY_NAME",
        "GET_ULAY_NAME",
        true,
        ArgumentShape::Tokens,
        QUERY,
    ),
    spec(
        "GET_OLAY_NAME",
        "GET_OLAY_NAME",
        true,
        ArgumentShape::Tokens,
        QUERY,
    ),
    spec(
        "SET_SPM_XYZ",
        "SET_SPM_XYZ",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec("SET_IJK", "SET_IJK", true, ArgumentShape::Tokens, STATE),
    spec("GET_IJK", "GET_IJK", true, ArgumentShape::Tokens, QUERY),
    spec("SET_INDEX", "SET_INDEX", true, ArgumentShape::Tokens, STATE),
    spec(
        "SET_XHAIRS_GAP",
        "SET_XHAIR_GAP",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_XHAIRS",
        "SET_XHAIRS",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_CROSSHAIRS",
        "SET_XHAIRS",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_XHAIR_GAP",
        "SET_XHAIR_GAP",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_OUTPLUG",
        "SET_OUTPLUG",
        false,
        ArgumentShape::Remainder,
        WRITE_STATE,
    ),
    spec(
        "OPEN_GRAPH_XY",
        "OPEN_GRAPH_XY",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "CLOSE_GRAPH_XY",
        "CLOSE_GRAPH_XY",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "CLEAR_GRAPH_XY",
        "CLEAR_GRAPH_XY",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "ADDTO_GRAPH_XY",
        "ADDTO_GRAPH_XY",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "OPEN_GRAPH_1D",
        "OPEN_GRAPH_1D",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "CLOSE_GRAPH_1D",
        "CLOSE_GRAPH_1D",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "CLEAR_GRAPH_1D",
        "CLEAR_GRAPH_1D",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "ADDTO_GRAPH_1D",
        "ADDTO_GRAPH_1D",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_GRAPH_GEOM",
        "SET_GRAPH_GEOM",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec("QUITT", "QUITT", false, ArgumentShape::None, PROCESS),
    spec("QQUIT", "QUITT", false, ArgumentShape::None, PROCESS),
    spec("QUIT", "QUIT", false, ArgumentShape::None, PROCESS),
    spec("SYSTEM", "SYSTEM", false, ArgumentShape::Remainder, SHELL),
    spec(
        "CHDIR",
        "CHDIR",
        false,
        ArgumentShape::Remainder,
        ENVIRONMENT,
    ),
    spec(
        "ADD_OVERLAY_COLOR",
        "ADD_OVERLAY_COLOR",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_THRESHOLD",
        "SET_THRESHOLD",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_THRESHNEW",
        "SET_THRESHNEW",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_FUNC_THRESH",
        "SET_THRESHOLD",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_PBAR_NUMBER",
        "SET_PBAR_NUMBER",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_PBAR_SIGN",
        "SET_PBAR_SIGN",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_PBAR_ALL",
        "SET_PBAR_ALL",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "PBAR_ROTATE",
        "PBAR_ROTATE",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "PBAR_SAVEIM",
        "PBAR_SAVEIM",
        true,
        ArgumentShape::Tokens,
        WRITE,
    ),
    spec(
        "SET_FUNC_AUTORANGE",
        "SET_FUNC_AUTORANGE",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_FUNC_RANGE",
        "SET_FUNC_RANGE",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_FUNC_VISIBLE",
        "SET_FUNC_VISIBLE",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_FUNC_ALPHA",
        "SET_FUNC_ALPHA",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_FUNC_BOXED",
        "SET_FUNC_BOXED",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SEE_OVERLAY",
        "SET_FUNC_VISIBLE",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SET_FUNC_RESAM",
        "SET_FUNC_RESAM",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec("SLEEP", "SLEEP", false, ArgumentShape::Tokens, PROCESS),
    spec(
        "SETENV",
        "SETENV",
        false,
        ArgumentShape::Tokens,
        ENVIRONMENT,
    ),
    spec("GETENV", "GETENV", false, ArgumentShape::Tokens, QUERY),
    spec(
        "DEFINE_COLORSCALE",
        "DEFINE_COLORSCALE",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "DEFINE_COLOR_SCALE",
        "DEFINE_COLORSCALE",
        false,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "OPEN_PANEL",
        "OPEN_PANEL",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "CLOSE_PANEL",
        "CLOSE_PANEL",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec("INSTACORR", "INSTACORR", true, ArgumentShape::Tokens, STATE),
    spec("REDISPLAY", "REDISPLAY", false, ArgumentShape::None, STATE),
    spec("REDRAW", "REDISPLAY", false, ArgumentShape::None, STATE),
    spec(
        "READ_NIML_FILE",
        "READ_NIML_FILE",
        false,
        ArgumentShape::Remainder,
        READ_STATE,
    ),
    spec(
        "READ_NIML_DATA",
        "READ_NIML_FILE",
        false,
        ArgumentShape::Remainder,
        READ_STATE,
    ),
    spec("TRACE", "TRACE", false, ArgumentShape::Tokens, STATE),
    spec(
        "QUIET_PLUGOUTS",
        "QUIET_PLUGOUTS",
        false,
        ArgumentShape::None,
        STATE,
    ),
    spec(
        "NOISY_PLUGOUTS",
        "NOISY_PLUGOUTS",
        false,
        ArgumentShape::None,
        STATE,
    ),
    spec(
        "SET_FUNC_PERCENTILE",
        "SET_FUNC_PERCENTILE",
        true,
        ArgumentShape::Tokens,
        STATE,
    ),
    spec(
        "SAVE_OVERLAY",
        "SAVE_OVERLAY",
        true,
        ArgumentShape::Tokens,
        WRITE,
    ),
    spec(
        "WRITE_OVERLAY",
        "SAVE_OVERLAY",
        true,
        ArgumentShape::Tokens,
        WRITE,
    ),
    spec(
        "SAVE_UNDERLAY",
        "SAVE_UNDERLAY",
        true,
        ArgumentShape::Tokens,
        WRITE,
    ),
    spec(
        "WRITE_UNDERLAY",
        "SAVE_UNDERLAY",
        true,
        ArgumentShape::Tokens,
        WRITE,
    ),
    spec(
        "WRITE_UNDERLAY",
        "SAVE_UNDERLAY",
        true,
        ArgumentShape::Tokens,
        WRITE,
    ),
    spec(
        "WRITE_CONT_SPX_HELP",
        "WRITE_CONT_SPX_HELP",
        false,
        ArgumentShape::Remainder,
        WRITE,
    ),
    spec(
        "SNAP_CONT",
        "SNAP_CONT",
        false,
        ArgumentShape::Remainder,
        WRITE,
    ),
    spec(
        "SNAP_VIEWER",
        "SNAP_VIEWER",
        true,
        ArgumentShape::Window,
        WRITE,
    ),
    spec(
        "SET_ULAY_RANGE",
        "SET_ULAY_RANGE",
        true,
        ArgumentShape::Window,
        STATE,
    ),
    spec(
        "DO_NOTHING",
        "DO_NOTHING",
        false,
        ArgumentShape::None,
        Effects::NONE,
    ),
];

/// Find the first compiled-in command with an exact, case-sensitive label.
pub fn static_command(name: &str) -> Option<&'static CommandSpec> {
    STATIC_COMMANDS.iter().find(|spec| spec.name == name)
}

/// Plugin command documented by AFNI but absent from the static table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocumentedDynamicCommand {
    /// Runtime-registered label.
    pub name: &'static str,
    /// Plugin that registers it.
    pub plugin: &'static str,
    /// Documented effects.
    pub effects: Effects,
}

/// Runtime driver commands documented in `README.driver`.
pub static DOCUMENTED_DYNAMIC_COMMANDS: &[DocumentedDynamicCommand] = &[DocumentedDynamicCommand {
    name: "DATASET#N",
    plugin: "Dataset#N",
    effects: STATE,
}];

/// One application-registered extension command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredCommand {
    /// Case-sensitive registered label.
    pub name: String,
    /// Whether the extension uses the conventional controller prefix.
    pub controller: bool,
    /// Broad argument form.
    pub arguments: ArgumentShape,
    /// Effects declared by the registering application.
    pub effects: Effects,
}

/// Dynamic command registry mirroring `AFNI_driver_register` name rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    commands: Vec<RegisteredCommand>,
}

impl Registry {
    /// Empty dynamic registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry containing commands from [`DOCUMENTED_DYNAMIC_COMMANDS`].
    pub fn with_documented_plugins() -> Self {
        let commands = DOCUMENTED_DYNAMIC_COMMANDS
            .iter()
            .map(|command| RegisteredCommand {
                name: command.name.to_owned(),
                controller: false,
                arguments: ArgumentShape::Tokens,
                effects: command.effects,
            })
            .collect();
        Self { commands }
    }

    /// Register a command. Names must contain no whitespace and may not
    /// duplicate a static or already registered label.
    pub fn register(&mut self, command: RegisteredCommand) -> Result<(), RegistryError> {
        if command.name.is_empty() || command.name.chars().any(char::is_whitespace) {
            return Err(RegistryError::InvalidName(command.name));
        }
        if static_command(&command.name).is_some()
            || self.commands.iter().any(|known| known.name == command.name)
        {
            return Err(RegistryError::Duplicate(command.name));
        }
        self.commands.push(command);
        Ok(())
    }

    /// Commands in registration order, which is also AFNI's dispatch order
    /// after the static table.
    pub fn commands(&self) -> &[RegisteredCommand] {
        &self.commands
    }
}

/// Dynamic registry error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// Empty or whitespace-containing labels are illegal in AFNI.
    #[error("illegal DRIVE_AFNI registration name {0:?}")]
    InvalidName(String),
    /// Static and dynamic labels cannot be duplicated.
    #[error("duplicate DRIVE_AFNI registration {0:?}")]
    Duplicate(String),
}

/// Controller identifier accepted by current AFNI (`A` through `J`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Controller {
    label: char,
}

impl Controller {
    /// Parse an uppercase AFNI controller label.
    pub fn new(label: char) -> Option<Self> {
        ('A'..='J').contains(&label).then_some(Self { label })
    }

    /// Controller label.
    pub const fn label(self) -> char {
        self.label
    }

    /// Zero-based controller index.
    pub const fn index(self) -> usize {
        self.label as usize - 'A' as usize
    }
}

/// Typed AFNI window name, retaining unknown/plugin names losslessly.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WindowName {
    /// Axial image viewer.
    AxialImage,
    /// Sagittal image viewer.
    SagittalImage,
    /// Coronal image viewer.
    CoronalImage,
    /// Axial graph viewer.
    AxialGraph,
    /// Sagittal graph viewer.
    SagittalGraph,
    /// Coronal graph viewer.
    CoronalGraph,
    /// All three image viewers.
    AllImage,
    /// All relevant open viewers for commands that support it.
    All,
    /// `plugin.<buttonlabel>` target.
    Plugin(String),
    /// Receiver-specific or unrecognised window text.
    Other(String),
}

impl WindowName {
    fn parse(value: &str) -> Self {
        let lower = value.to_ascii_lowercase();
        if lower.contains("allimage") || lower.contains("all_image") {
            Self::AllImage
        } else if lower == "all" {
            Self::All
        } else if lower.contains("axialim") || lower.contains("axial_im") {
            Self::AxialImage
        } else if lower.contains("sagittalim")
            || lower.contains("sagittal_im")
            || lower.contains("sagitalim")
            || lower.contains("sagital_im")
        {
            Self::SagittalImage
        } else if lower.contains("coronalim") || lower.contains("coronal_im") {
            Self::CoronalImage
        } else if lower.contains("axialgr") || lower.contains("axial_gr") {
            Self::AxialGraph
        } else if lower.contains("sagittalgr")
            || lower.contains("sagittal_gr")
            || lower.contains("sagitalgr")
            || lower.contains("sagital_gr")
        {
            Self::SagittalGraph
        } else if lower.contains("coronalgr") || lower.contains("coronal_gr") {
            Self::CoronalGraph
        } else if lower.contains("plugin.") {
            Self::Plugin(value.to_owned())
        } else {
            Self::Other(value.to_owned())
        }
    }
}

/// A typed value paired with its exact source span.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Spanned<T> {
    /// Parsed value.
    pub value: T,
    /// Byte range in the original source.
    pub span: Span,
}

/// Semantic content of one quote-aware argument.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ArgumentValue {
    /// Ordinary positional token.
    Word(String),
    /// `name=value` compound option.
    Assignment {
        /// Text before the first equals sign.
        name: String,
        /// Text after the first equals sign.
        value: String,
    },
}

/// One argument token. Its span always reconstructs the exact raw spelling.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Argument {
    /// Semantic value with matching quotes removed.
    pub value: ArgumentValue,
    /// Complete raw token span, including quotes.
    pub span: Span,
    /// Whether any single or double quoting occurred inside the token.
    pub quoted: bool,
}

impl Argument {
    /// Semantic text, joining assignment name/value with `=` when needed.
    pub fn text(&self) -> String {
        match &self.value {
            ArgumentValue::Word(value) => value.clone(),
            ArgumentValue::Assignment { name, value } => format!("{name}={value}"),
        }
    }
}

/// How a command was recognized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recognition {
    /// Entry in AFNI's compiled-in table.
    Static(&'static CommandSpec),
    /// Entry supplied through a runtime [`Registry`].
    Dynamic(RegisteredCommand),
}

impl Recognition {
    /// Invoked command label.
    pub fn name(&self) -> &str {
        match self {
            Self::Static(spec) => spec.name,
            Self::Dynamic(command) => &command.name,
        }
    }

    /// Canonical label. Dynamic labels canonicalize to themselves.
    pub fn canonical(&self) -> &str {
        match self {
            Self::Static(spec) => spec.canonical,
            Self::Dynamic(command) => &command.name,
        }
    }

    /// Declared effects.
    pub fn effects(&self) -> Effects {
        match self {
            Self::Static(spec) => spec.effects,
            Self::Dynamic(command) => command.effects,
        }
    }

    fn controller(&self) -> bool {
        match self {
            Self::Static(spec) => spec.controller,
            Self::Dynamic(command) => command.controller,
        }
    }

    fn argument_shape(&self) -> ArgumentShape {
        match self {
            Self::Static(spec) => spec.arguments,
            Self::Dynamic(command) => command.arguments,
        }
    }
}

/// A recognized static or registered command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownCommand {
    /// Static or dynamic descriptor.
    pub recognition: Recognition,
    /// Parsed optional controller prefix.
    pub controller: Option<Spanned<Controller>>,
    /// Typed first window argument for window-oriented commands.
    pub window: Option<Spanned<WindowName>>,
    /// Quote-aware semantic arguments after any controller prefix.
    pub arguments: Vec<Argument>,
}

impl KnownCommand {
    /// Canonical static label or the registered dynamic label.
    pub fn canonical_name(&self) -> &str {
        self.recognition.canonical()
    }

    /// Effects and risk metadata.
    pub fn effects(&self) -> Effects {
        self.recognition.effects()
    }

    /// Conservative risk tier.
    pub fn risk(&self) -> Risk {
        self.effects().risk()
    }
}

/// Unknown command preserved for transmission to legacy AFNI/plugins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCommand {
    /// First whitespace-delimited text, if present.
    pub name: String,
    /// Exact source span of `name`.
    pub name_span: Span,
    /// Exact unparsed remainder after the name.
    pub arguments: String,
    /// Exact source span of `arguments`.
    pub argument_span: Span,
}

/// Parsed command body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Compiled-in or explicitly registered command.
    Known(KnownCommand),
    /// Unknown command kept byte-for-byte for a compatible receiver.
    Raw(RawCommand),
}

impl Command {
    /// Effects for policy checks. Raw commands are deliberately unknown.
    pub fn risk(&self) -> Risk {
        match self {
            Self::Known(command) => command.risk(),
            Self::Raw(_) => Risk::Unknown,
        }
    }
}

/// Complete lossless parse result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    source: String,
    /// Optional exact `DRIVE_AFNI ` prefix at byte zero.
    pub prefix_span: Option<Span>,
    /// Trimmed command payload as AFNI dispatch sees it.
    pub payload_span: Span,
    /// Matched label (or raw first word) in the original source.
    pub command_span: Span,
    /// Raw argument remainder after the dispatched label.
    pub argument_span: Span,
    /// Typed or raw command body.
    pub command: Command,
}

impl ParsedCommand {
    /// Original input, including prefix and whitespace.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Exact trimmed command payload suitable for raw transmission.
    pub fn wire_command(&self) -> &str {
        self.payload_span.slice(&self.source)
    }

    /// Exact raw argument remainder.
    pub fn raw_arguments(&self) -> &str {
        self.argument_span.slice(&self.source)
    }
}

/// AFNI dispatch matching policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DispatchMode {
    /// Reproduce `afni_driver.c`: case-sensitive, source-ordered prefix match.
    AfniCompatible,
    /// Require the first whitespace-delimited token to equal a label.
    Exact,
}

/// Policy for labels absent from both static and dynamic tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnknownPolicy {
    /// Preserve unknown text as [`Command::Raw`].
    Preserve,
    /// Return [`ParseErrorCode::UnknownCommand`].
    Reject,
}

/// Parser configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParseOptions {
    /// Dispatch matching behavior.
    pub dispatch: DispatchMode,
    /// Unknown command handling.
    pub unknown: UnknownPolicy,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self {
            dispatch: DispatchMode::AfniCompatible,
            unknown: UnknownPolicy::Preserve,
        }
    }
}

/// Stable category for structural parse failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParseErrorCode {
    /// Input is empty after AFNI-compatible trimming.
    Empty,
    /// A single or double quote was not closed.
    UnterminatedQuote,
    /// Closed-vocabulary parsing rejected an unknown label.
    UnknownCommand,
}

/// Structural parse error with an exact source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Stable machine-readable category.
    pub code: ParseErrorCode,
    /// Relevant byte range in the original input.
    pub span: Span,
    /// Human-readable context.
    pub detail: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "DRIVE_AFNI parse error {:?} at {}..{}: {}",
            self.code, self.span.start, self.span.end, self.detail
        )
    }
}

impl std::error::Error for ParseError {}

/// Parse with AFNI-compatible dispatch and raw preservation.
pub fn parse(source: impl Into<String>) -> Result<ParsedCommand, ParseError> {
    parse_with(source, &Registry::new(), ParseOptions::default())
}

/// Parse using an explicit dynamic registry and policy.
pub fn parse_with(
    source: impl Into<String>,
    registry: &Registry,
    options: ParseOptions,
) -> Result<ParsedCommand, ParseError> {
    let source = source.into();
    let prefix_span = source
        .starts_with("DRIVE_AFNI ")
        .then_some(Span::new(0, "DRIVE_AFNI ".len()));
    let payload_start = prefix_span.map_or(0, |span| span.end);
    let payload_span = trim_span(&source, Span::new(payload_start, source.len()));
    if payload_span.is_empty() {
        return Err(ParseError {
            code: ParseErrorCode::Empty,
            span: payload_span,
            detail: "command is empty".into(),
        });
    }

    let payload = payload_span.slice(&source);
    let matched = instacorr_patch(payload, payload_span.start)
        .or_else(|| match_static(payload, payload_span.start, options.dispatch))
        .or_else(|| match_dynamic(payload, payload_span.start, registry, options.dispatch));

    let Some((recognition, command_span, argument_start)) = matched else {
        return parse_raw(source, prefix_span, payload_span, options.unknown);
    };
    let argument_span = trim_span(
        &source,
        Span::new(
            skip_whitespace(&source, argument_start, payload_span.end),
            payload_span.end,
        ),
    );
    let (controller, semantic_span) = if recognition.controller() {
        parse_controller(&source, argument_span)
    } else {
        (None, argument_span)
    };
    let arguments = tokenize(&source, semantic_span)?;
    let window = if recognition.argument_shape() == ArgumentShape::Window {
        arguments.first().map(|argument| Spanned {
            value: WindowName::parse(&argument.text()),
            span: argument.span,
        })
    } else {
        None
    };
    Ok(ParsedCommand {
        source,
        prefix_span,
        payload_span,
        command_span,
        argument_span,
        command: Command::Known(KnownCommand {
            recognition,
            controller,
            window,
            arguments,
        }),
    })
}

fn match_static(
    payload: &str,
    base: usize,
    mode: DispatchMode,
) -> Option<(Recognition, Span, usize)> {
    let word_len = first_word_len(payload);
    STATIC_COMMANDS.iter().find_map(|spec| {
        let matches = match mode {
            DispatchMode::AfniCompatible => payload.starts_with(spec.name),
            DispatchMode::Exact => word_len == spec.name.len() && payload.starts_with(spec.name),
        };
        matches.then(|| {
            (
                Recognition::Static(spec),
                Span::new(base, base + spec.name.len()),
                base + spec.name.len(),
            )
        })
    })
}

fn match_dynamic(
    payload: &str,
    base: usize,
    registry: &Registry,
    mode: DispatchMode,
) -> Option<(Recognition, Span, usize)> {
    let word_len = first_word_len(payload);
    registry.commands().iter().find_map(|command| {
        let matches = match mode {
            DispatchMode::AfniCompatible => payload.starts_with(&command.name),
            DispatchMode::Exact => {
                word_len == command.name.len() && payload.starts_with(&command.name)
            }
        };
        matches.then(|| {
            let len = command.name.len();
            (
                Recognition::Dynamic(command.clone()),
                Span::new(base, base + len),
                base + len,
            )
        })
    })
}

fn instacorr_patch(payload: &str, base: usize) -> Option<(Recognition, Span, usize)> {
    let suffix_start =
        if payload.starts_with("INSTACORR_SET") || payload.starts_with("INSTACORR_INIT") {
            Some("INSTACORR_".len())
        } else {
            None
        }?;
    let spec = static_command("INSTACORR").expect("INSTACORR is a static command");
    Some((
        Recognition::Static(spec),
        Span::new(base, base + "INSTACORR".len()),
        base + suffix_start,
    ))
}

fn parse_raw(
    source: String,
    prefix_span: Option<Span>,
    payload_span: Span,
    policy: UnknownPolicy,
) -> Result<ParsedCommand, ParseError> {
    let payload = payload_span.slice(&source);
    let word_len = first_word_len(payload);
    let command_span = Span::new(payload_span.start, payload_span.start + word_len);
    let argument_start = skip_whitespace(&source, command_span.end, payload_span.end);
    let argument_span = Span::new(argument_start, payload_span.end);
    if policy == UnknownPolicy::Reject {
        return Err(ParseError {
            code: ParseErrorCode::UnknownCommand,
            span: command_span,
            detail: format!("unknown command {:?}", command_span.slice(&source)),
        });
    }
    let raw = RawCommand {
        name: command_span.slice(&source).to_owned(),
        name_span: command_span,
        arguments: argument_span.slice(&source).to_owned(),
        argument_span,
    };
    Ok(ParsedCommand {
        source,
        prefix_span,
        payload_span,
        command_span,
        argument_span,
        command: Command::Raw(raw),
    })
}

fn parse_controller(source: &str, span: Span) -> (Option<Spanned<Controller>>, Span) {
    let bytes = source.as_bytes();
    if span.is_empty() || !bytes[span.start].is_ascii_uppercase() {
        return (None, span);
    }
    let label = bytes[span.start] as char;
    let Some(controller) = Controller::new(label) else {
        return (None, span);
    };
    let next = span.start + 1;
    let semantic_start = if next == span.end {
        next
    } else if bytes[next] == b'.' || bytes[next] == b' ' {
        skip_whitespace(source, next + 1, span.end)
    } else {
        return (None, span);
    };
    (
        Some(Spanned {
            value: controller,
            span: Span::new(span.start, next),
        }),
        Span::new(semantic_start, span.end),
    )
}

fn tokenize(source: &str, span: Span) -> Result<Vec<Argument>, ParseError> {
    let mut arguments = Vec::new();
    let mut index = span.start;
    while index < span.end {
        index = skip_whitespace(source, index, span.end);
        if index == span.end {
            break;
        }
        let start = index;
        let mut quote: Option<(u8, usize)> = None;
        let mut value = Vec::new();
        let mut quoted = false;
        while index < span.end {
            let byte = source.as_bytes()[index];
            match quote {
                Some((mark, _)) if byte == mark => {
                    quote = None;
                    quoted = true;
                    index += 1;
                }
                Some(_) => {
                    value.push(byte);
                    index += 1;
                }
                None if byte == b'\'' || byte == b'"' => {
                    quote = Some((byte, index));
                    quoted = true;
                    index += 1;
                }
                None if byte.is_ascii_whitespace() => break,
                None => {
                    value.push(byte);
                    index += 1;
                }
            }
        }
        if let Some((_, opening)) = quote {
            return Err(ParseError {
                code: ParseErrorCode::UnterminatedQuote,
                span: Span::new(opening, (opening + 1).min(source.len())),
                detail: "quoted argument is not closed".into(),
            });
        }
        let value = String::from_utf8(value).expect("tokens retain source UTF-8 byte sequences");
        let parsed = if let Some((name, assignment_value)) = value.split_once('=') {
            if !name.is_empty() {
                ArgumentValue::Assignment {
                    name: name.to_owned(),
                    value: assignment_value.to_owned(),
                }
            } else {
                ArgumentValue::Word(value)
            }
        } else {
            ArgumentValue::Word(value)
        };
        arguments.push(Argument {
            value: parsed,
            span: Span::new(start, index),
            quoted,
        });
    }
    Ok(arguments)
}

fn trim_span(source: &str, span: Span) -> Span {
    let bytes = source.as_bytes();
    let mut start = span.start;
    let mut end = span.end;
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    Span::new(start, end)
}

fn skip_whitespace(source: &str, mut start: usize, end: usize) -> usize {
    let bytes = source.as_bytes();
    while start < end && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    start
}

fn first_word_len(text: &str) -> usize {
    text.as_bytes()
        .iter()
        .position(|byte| byte.is_ascii_whitespace())
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(parsed: &ParsedCommand) -> &KnownCommand {
        match &parsed.command {
            Command::Known(command) => command,
            Command::Raw(command) => panic!("unexpected raw command {command:?}"),
        }
    }

    #[test]
    fn preserves_source_prefix_and_spans() {
        let parsed = parse("DRIVE_AFNI   SWITCH_OVERLAY A.stats+tlrc   ").unwrap();
        assert_eq!(
            parsed.source(),
            "DRIVE_AFNI   SWITCH_OVERLAY A.stats+tlrc   "
        );
        assert_eq!(parsed.wire_command(), "SWITCH_OVERLAY A.stats+tlrc");
        assert_eq!(parsed.command_span.slice(parsed.source()), "SWITCH_OVERLAY");
        assert_eq!(parsed.raw_arguments(), "A.stats+tlrc");
        assert_eq!(known(&parsed).canonical_name(), "SET_FUNCTION");
        assert_eq!(
            known(&parsed).controller.as_ref().unwrap().value.label(),
            'A'
        );
        assert_eq!(known(&parsed).arguments[0].text(), "stats+tlrc");
    }

    #[test]
    fn quotes_and_compound_options_are_typed_but_lossless() {
        let source = r#"OPEN_WINDOW B.axialimage overlay_label="some string" keypress=Z ''"#;
        let parsed = parse(source).unwrap();
        let command = known(&parsed);
        assert_eq!(
            command.window.as_ref().unwrap().value,
            WindowName::AxialImage
        );
        assert_eq!(command.arguments.len(), 4);
        assert_eq!(command.arguments[1].text(), "overlay_label=some string");
        assert_eq!(command.arguments[2].text(), "keypress=Z");
        assert_eq!(command.arguments[3].text(), "");
        assert_eq!(
            command.arguments[1].span.slice(parsed.source()),
            r#"overlay_label="some string""#
        );
        assert_eq!(parsed.wire_command(), source);
    }

    #[test]
    fn afni_prefix_matching_and_exact_mode_differ() {
        let compatible = parse("REDRAW_extra").unwrap();
        assert_eq!(known(&compatible).canonical_name(), "REDISPLAY");
        assert_eq!(known(&compatible).arguments[0].text(), "_extra");

        let exact = parse_with(
            "REDRAW_extra",
            &Registry::new(),
            ParseOptions {
                dispatch: DispatchMode::Exact,
                unknown: UnknownPolicy::Preserve,
            },
        )
        .unwrap();
        assert!(matches!(exact.command, Command::Raw(_)));
        assert!(matches!(parse("redraw").unwrap().command, Command::Raw(_)));
    }

    #[test]
    fn source_order_protects_xhairs_gap_and_instacorr_patch() {
        let gap = parse("SET_XHAIRS_GAP A.4").unwrap();
        assert_eq!(known(&gap).canonical_name(), "SET_XHAIR_GAP");

        let instacorr = parse("INSTACORR_SET 1 2 3 J").unwrap();
        assert_eq!(known(&instacorr).canonical_name(), "INSTACORR");
        assert_eq!(known(&instacorr).arguments[0].text(), "SET");
        assert_eq!(instacorr.wire_command(), "INSTACORR_SET 1 2 3 J");
    }

    #[test]
    fn dynamic_and_unknown_commands_remain_distinct() {
        let registry = Registry::with_documented_plugins();
        let parsed = parse_with("DATASET#N epi::red", &registry, ParseOptions::default()).unwrap();
        assert!(matches!(
            known(&parsed).recognition,
            Recognition::Dynamic(_)
        ));
        assert_eq!(known(&parsed).arguments[0].text(), "epi::red");

        let raw = parse("NIMH_FUTURE thing='kept exactly'").unwrap();
        assert_eq!(raw.wire_command(), "NIMH_FUTURE thing='kept exactly'");
        assert_eq!(raw.command.risk(), Risk::Unknown);
        assert!(matches!(raw.command, Command::Raw(_)));
    }

    #[test]
    fn policy_metadata_marks_dangerous_commands() {
        assert_eq!(known(&parse("GET_IJK").unwrap()).risk(), Risk::Observe);
        assert_eq!(
            known(&parse("SAVE_PNG axialimage x.png").unwrap()).risk(),
            Risk::FileWrite
        );
        assert_eq!(
            known(&parse("SET_SESSION A.somewhere").unwrap()).risk(),
            Risk::FileRead
        );
        assert_eq!(known(&parse("QUIT").unwrap()).risk(), Risk::ProcessControl);
        assert_eq!(
            known(&parse("SETENV NAME VALUE").unwrap()).risk(),
            Risk::Environment
        );
        assert_eq!(
            known(&parse("SYSTEM rm something").unwrap()).risk(),
            Risk::ArbitraryCode
        );
    }

    #[test]
    fn structural_errors_have_codes_and_spans() {
        let empty = parse("DRIVE_AFNI   ").unwrap_err();
        assert_eq!(empty.code, ParseErrorCode::Empty);

        let quote = parse("SYSTEM echo 'unfinished").unwrap_err();
        assert_eq!(quote.code, ParseErrorCode::UnterminatedQuote);
        assert_eq!(quote.span.slice("SYSTEM echo 'unfinished"), "'");

        let unknown = parse_with(
            "FUTURE_PLUGIN x",
            &Registry::new(),
            ParseOptions {
                dispatch: DispatchMode::Exact,
                unknown: UnknownPolicy::Reject,
            },
        )
        .unwrap_err();
        assert_eq!(unknown.code, ParseErrorCode::UnknownCommand);
        assert_eq!(unknown.span.slice("FUTURE_PLUGIN x"), "FUTURE_PLUGIN");
    }
}
