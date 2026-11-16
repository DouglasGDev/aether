pub mod approval;
pub mod catalog;
pub mod instalador;
pub mod intent;
pub mod manager;
pub mod tool;

pub use approval::{ApprovalBroker, ApprovalOutcome, ApprovalRequest};
pub use catalog::{CatalogBudget, CatalogEntry};
pub use instalador::{Escopo, Instalacoes, ServidorEvent, ServidorPronto};
pub use manager::{McpEvent, McpManager};
pub use tool::ToolInfo;