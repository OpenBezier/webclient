use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum ConnectorError {
    #[error("ws connect error: {0}")]
    WsConnectError(String),
    #[error("ws read data error: {0}")]
    WsReadError(String),
    #[error("unknown data store error")]
    Unknown,
}
