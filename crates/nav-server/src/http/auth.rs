#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAuthToken(String);

impl LocalAuthToken {
    pub fn new_unchecked(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose_for_bootstrap(&self) -> &str {
        &self.0
    }
}
