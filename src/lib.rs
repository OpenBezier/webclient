use std::any::Any;
use std::sync::Arc;

pub mod ws;
pub use ws::*;

pub mod error;
pub use error::*;

pub mod command;
pub mod request;

#[derive(Debug, Clone)]
pub enum DataEntity {
    Binary { data: Vec<u8> },
    Text { data: String },
    Error { err: ConnectorError },
    Success,
}

use async_trait::async_trait;
// #[async_trait(?Send)]
#[async_trait]
pub trait ClientCallback: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    async fn process(&self, data: DataEntity, callback: Arc<dyn ClientCallback>, client: &WsClient);
}
