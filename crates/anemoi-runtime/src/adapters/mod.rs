pub mod http_inspect;
pub mod llama_cpp;
pub mod ollama;

pub use http_inspect::HttpInspectAdapter;
pub use llama_cpp::LlamaCppAdapter;
pub use ollama::OllamaAdapter;
