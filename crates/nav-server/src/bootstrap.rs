#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendEndpoint {
    pub base_url: String,
    pub auth_token: String,
}
