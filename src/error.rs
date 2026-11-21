use std::fmt;

#[derive(Debug, Clone)]
pub enum YsptpError {
    Msg(String),

    DeadHost(String),
}

impl YsptpError {
    pub fn new<S: Into<String>>(s: S) -> Self {
        YsptpError::Msg(s.into())
    }

    pub fn dead_host<S: Into<String>>(s: S) -> Self {
        YsptpError::DeadHost(s.into())
    }

    pub fn message(&self) -> &str {
        match self {
            YsptpError::Msg(s) | YsptpError::DeadHost(s) => s,
        }
    }

    pub fn is_dead_host(&self) -> bool {
        matches!(self, YsptpError::DeadHost(_))
    }
}

impl fmt::Display for YsptpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for YsptpError {}

pub type Result<T> = std::result::Result<T, YsptpError>;

fn error_chain(e: &reqwest::Error) -> String {
    let mut out = e.to_string();
    let mut cur: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(c) = cur {
        out.push_str(" <- ");
        out.push_str(&c.to_string());
        cur = c.source();
    }
    out
}

pub fn normalize_network_error(e: &reqwest::Error) -> String {
    let chain = error_chain(e);
    if e.is_timeout() {
        return format!("operation timed out: {chain}");
    }
    if e.is_connect() {
        return format!("connection refused: {chain}");
    }
    format!("network error: {chain}")
}
