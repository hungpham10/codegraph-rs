use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Lỗi parse kind từ chuỗi không hợp lệ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidKind(pub String);

impl std::fmt::Display for InvalidKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid kind: {}", self.0)
    }
}

impl std::error::Error for InvalidKind {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    File,
    Module,
    Class,
    Struct,
    Interface,
    Trait,
    Protocol,
    Function,
    Method,
    Property,
    Field,
    Variable,
    Constant,
    Enum,
    EnumMember,
    TypeAlias,
    Namespace,
    Parameter,
    Import,
    Export,
    Route,
    Component,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    Contains,
    Calls,
    Imports,
    Exports,
    Extends,
    Implements,
    References,
    TypeOf,
    Returns,
    Instantiates,
    Overrides,
    Decorates,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Module => "module",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Protocol => "protocol",
            Self::Function => "function",
            Self::Method => "method",
            Self::Property => "property",
            Self::Field => "field",
            Self::Variable => "variable",
            Self::Constant => "constant",
            Self::Enum => "enum",
            Self::EnumMember => "enum_member",
            Self::TypeAlias => "type_alias",
            Self::Namespace => "namespace",
            Self::Parameter => "parameter",
            Self::Import => "import",
            Self::Export => "export",
            Self::Route => "route",
            Self::Component => "component",
        }
    }
}

impl FromStr for NodeKind {
    type Err = InvalidKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "file" => Self::File,
            "module" => Self::Module,
            "class" => Self::Class,
            "struct" => Self::Struct,
            "interface" => Self::Interface,
            "trait" => Self::Trait,
            "protocol" => Self::Protocol,
            "function" => Self::Function,
            "method" => Self::Method,
            "property" => Self::Property,
            "field" => Self::Field,
            "variable" => Self::Variable,
            "constant" => Self::Constant,
            "enum" => Self::Enum,
            "enum_member" => Self::EnumMember,
            "type_alias" => Self::TypeAlias,
            "namespace" => Self::Namespace,
            "parameter" => Self::Parameter,
            "import" => Self::Import,
            "export" => Self::Export,
            "route" => Self::Route,
            "component" => Self::Component,
            _ => return Err(InvalidKind(s.to_string())),
        })
    }
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contains => "contains",
            Self::Calls => "calls",
            Self::Imports => "imports",
            Self::Exports => "exports",
            Self::Extends => "extends",
            Self::Implements => "implements",
            Self::References => "references",
            Self::TypeOf => "type_of",
            Self::Returns => "returns",
            Self::Instantiates => "instantiates",
            Self::Overrides => "overrides",
            Self::Decorates => "decorates",
        }
    }
}

impl FromStr for EdgeKind {
    type Err = InvalidKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "contains" => Self::Contains,
            "calls" => Self::Calls,
            "imports" => Self::Imports,
            "exports" => Self::Exports,
            "extends" => Self::Extends,
            "implements" => Self::Implements,
            "references" => Self::References,
            "type_of" => Self::TypeOf,
            "returns" => Self::Returns,
            "instantiates" => Self::Instantiates,
            "overrides" => Self::Overrides,
            "decorates" => Self::Decorates,
            _ => return Err(InvalidKind(s.to_string())),
        })
    }
}
