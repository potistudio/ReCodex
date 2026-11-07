//! Model providers: the connection to the services that serve models.
//!
//! Every adapter implements [`crate::model::ModelClient`]. This layer does not depend on the
//! agent harness; `crate::connection` is what joins the two.
//!
//! - `gateway`: Model Gateway helpers shared by the adapters, and the Chat Completions adapter.
//! - `messages_api`, `responses_api`, `generate_api`: Anthropic Messages, OpenAI Responses,
//!   and Command Code CLI generation adapters.
//! - `command_code`: Command Code browser sign-in and credential storage.
//! - `command_code_client`: per-model routing and plan fallback for Command Code.
pub(crate) mod command_code;
pub(crate) mod command_code_client;
pub(crate) mod gateway;
mod generate_api;
mod messages_api;
mod responses_api;
