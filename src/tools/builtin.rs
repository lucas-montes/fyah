use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct ToolProperty {
    #[serde(rename = "type")]
    pub property_type: &'static str,
    pub description: &'static str,
}

macro_rules! count_fields {
    () => { 0usize };
    ($head:ident $(, $tail:ident)*) => { 1usize + count_fields!($($tail),*) };
}

#[macro_export]
macro_rules! define_builtin_tools {
    ( $( $variant:ident => { name: $name:expr, description: $desc:expr, properties: { $( $field:ident : $field_desc:expr ),* $(,)? } } ),* $(,)? ) => {
        paste::paste! {
            $(
                #[derive(Debug, Serialize)]
                pub struct [<$variant Properties>] {
                    $( pub $field: $crate::tools::builtin::ToolProperty ),*
                }

                #[derive(Debug, Serialize)]
                pub struct [<$variant Params>] {
                    #[serde(rename = "type")]
                    pub param_type: &'static str,
                    pub properties: [<$variant Properties>],
                    pub required: [&'static str; count_fields!($($field),*)],
                }

                impl [<$variant Properties>] {
                    pub const fn new() -> Self {
                        Self {
                            $( $field: $crate::tools::builtin::ToolProperty { property_type: "string", description: $field_desc } ),*
                        }
                    }
                }

                impl [<$variant Params>] {
                    pub const fn new() -> Self {
                        Self {
                            param_type: "object",
                            properties: [<$variant Properties>]::new(),
                            required: [ $( stringify!($field) ),* ],
                        }
                    }
                }

                #[derive(Debug, Deserialize)]
                pub struct [<$variant Args>] {
                    $( pub $field: String ),*
                }
            )*

            #[derive(Debug, PartialEq, Eq, Hash)]
            pub enum BuiltinTool {
                $( $variant ),*
            }

            impl BuiltinTool {
                pub fn name(&self) -> &'static str {
                    match self {
                        $( Self::$variant => $name ),*
                    }
                }
                pub fn description(&self) -> &'static str {
                    match self {
                        $( Self::$variant => $desc ),*
                    }
                }
                pub fn parameters(&self) -> serde_json::Value {
                    match self {
                        $(
                            Self::$variant => {
                                let params = [<$variant Params>]::new();
                                serde_json::to_value(&params).unwrap()
                            }
                        ),*
                    }
                }
                pub fn as_tool(&self) -> $crate::model::Tool {
                    $crate::model::Tool::new(self.name().to_string(), self.description().to_string(), self.parameters())
                }
                pub fn try_from_name(name: &str) -> Option<Self> {
                    match name {
                        $( $name => Some(Self::$variant), )*
                        _ => None,
                    }
                }
                pub fn all_variants() -> &'static [Self] {
                    &[ $( Self::$variant ),* ]
                }
                pub fn all_tools() -> Vec<$crate::model::Tool> {
                    Self::all_variants().iter().map(|t| t.as_tool()).collect()
                }
            }

            #[derive(Debug)]
            pub enum BuiltinToolCall {
                $( $variant { id: String, args: [<$variant Args>] } ),*
            }

            impl TryFrom<&$crate::model::ToolCall> for BuiltinToolCall {
                type Error = $crate::config::AgentError;
                fn try_from(call: &$crate::model::ToolCall) -> Result<Self, Self::Error> {
                    match call.name() {
                        $(
                            $name => {
                                let args: [<$variant Args>] = serde_json::from_str(call.arguments())
                                    .map_err(|e| $crate::config::AgentError::Tool(format!("invalid args for {}: {}", $name, e)))?;
                                Ok(Self::$variant { id: call.id().to_string(), args })
                            }
                        ),*
                        _ => Err($crate::config::AgentError::Tool(format!("unknown builtin tool: {}", call.name()))),
                    }
                }
            }

            impl BuiltinToolCall {
                pub fn name(&self) -> &'static str {
                    match self {
                        $( Self::$variant { .. } => $name ),*
                    }
                }
                pub fn id(&self) -> &str {
                    match self {
                        $( Self::$variant { id, .. } => id ),*
                    }
                }
            }
        }
    };
}

define_builtin_tools! {
    Read => {
        name: "Read",
        description: "Read and return the contents of a file",
        properties: {
            file_path: "The path to the file to read"
        }
    },
    Write => {
        name: "Write",
        description: "Write content to a file",
        properties: {
            file_path: "The path to the file to write",
            content: "The content to write to the file"
        }
    },
    Bash => {
        name: "Bash",
        description: "Execute a shell command",
        properties: {
            command: "The command to execute"
        }
    }
}
