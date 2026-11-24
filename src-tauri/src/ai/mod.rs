pub mod adaptadores;
pub mod alojamento;
pub mod cabecalho;
pub mod catalog;
pub mod chat;
pub mod compactacao;
pub mod derrame;
pub mod download;
pub mod engine;
pub mod geracao;
pub mod ggml;
pub mod granite;
pub mod memoria;
pub mod router;
pub mod session;

pub use chat::{Alvo, ChatMessage, ChatTemplate, ToolCall, ToolSpec};
pub use engine::{Acelerador, Alojado, Emission, Engine, Generation, SamplingParams, StopReason};
pub use session::{SessionEvent, TurnConfig, TurnOutcome, run_turn};