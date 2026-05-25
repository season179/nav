//! Local HTTP transport for frontend-to-backend communication.

pub mod auth;
pub mod rpc;
pub mod sse;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpServerConfig {
    pub bind_addr: String,
}

impl Default for HttpServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:0".to_string(),
        }
    }
}

#[derive(Debug)]
pub struct HttpServer {
    config: HttpServerConfig,
}

impl HttpServer {
    pub fn new(config: HttpServerConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &HttpServerConfig {
        &self.config
    }
}
