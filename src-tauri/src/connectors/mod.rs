pub mod meta;
pub mod registry;

pub use meta::{parse_qualified_name, qualified_tool_name, ToolMeta};
pub use registry::{
    ConnectUi, Connector, ConnectorCategory, ConnectorKind, DesktopOs, McpAuth, McpEntry,
    Registry, RegistryError, Resolution,
};