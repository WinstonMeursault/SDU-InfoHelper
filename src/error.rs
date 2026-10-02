//! Redacted errors shared by school service adapters.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("{0}")]
    Config(&'static str),
    #[error("登录凭据已失效，请在本机运行 auth login；首次登录可加 --trust-device。")]
    Authentication,
    #[error("{0}")]
    AuthFlow(&'static str),
    #[error("连接学校接口超时，未获得有效电量。")]
    Timeout,
    #[error("连接学校接口失败，未获得有效电量。")]
    Network,
    #[error("学校接口返回 HTTP {0}，未获得有效电量。")]
    Http(u16),
    #[error("{0}")]
    Response(&'static str),
}
