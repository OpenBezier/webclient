use super::WsClient;
use crossbeam::queue::SegQueue;
use dashmap::DashMap;
use futures::future::Future;
use futures::task::Poll;
use std::sync::Arc;
use std::task::Waker;

#[derive(Clone)]
pub struct ClientCommand<T: Send + Sync + serde::ser::Serialize> {
    pub event_id: String,
    pub queue: Arc<DashMap<String, Option<T>>>,
    pub wake_mgt: WakerManager,
}

impl<T: Send + Sync + serde::ser::Serialize> ClientCommand<T> {
    pub async fn send_indication(client: &WsClient, in_data: T) -> anyhow::Result<()> {
        let data = webproto::Indication::<T>::encode(in_data)?;
        let send_status = client.send_binary(data);
        if send_status.is_err() {
            return Err(anyhow::anyhow!(
                "send socket data to client with error: {:?}",
                send_status.err()
            ));
        }
        anyhow::Ok(())
    }

    pub async fn send_answer(
        client: &WsClient,
        in_data: T,
        event_id: String,
    ) -> anyhow::Result<()> {
        let data = webproto::ServerCommand::<T>::encode(in_data, event_id)?;
        let send_status = client.send_binary(data);
        if send_status.is_err() {
            return Err(anyhow::anyhow!(
                "send socket data to client with error: {:?}",
                send_status.err()
            ));
        }
        anyhow::Ok(())
    }

    pub async fn send_command(
        client: &WsClient,
        in_data: T,
        timeout_seconds: u64,
        queue: &Arc<DashMap<String, Option<T>>>,
        wake_mgt: &WakerManager,
    ) -> anyhow::Result<T> {
        let event_id = uuid::Uuid::new_v4().to_string();
        let data = webproto::ClientCommand::<T>::encode(in_data, event_id.clone())?;
        queue.insert(event_id.clone(), None);

        // tracing::info!("send command");
        let send_status = client.send_binary(data);
        if send_status.is_err() {
            queue.remove(&event_id);
            return Err(anyhow::anyhow!(
                "send socket data to client with error: {:?}",
                send_status.err()
            ));
        }

        let client = ClientCommand {
            event_id: event_id.clone(),
            queue: queue.clone(),
            wake_mgt: wake_mgt.clone(),
        };

        let resp =
            tokio::time::timeout(tokio::time::Duration::from_secs(timeout_seconds), client).await;
        match resp {
            Ok(resp) => return anyhow::Ok(resp),
            Err(_) => {
                queue.remove(&event_id);
                return Err(anyhow::anyhow!("timeout for waiting for client response"));
            }
        }
    }
}

impl<T: Send + Sync + serde::ser::Serialize> Future for ClientCommand<T> {
    type Output = T;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        let Self {
            event_id,
            queue,
            wake_mgt,
        } = &mut *self;

        let value = queue.entry(event_id.clone());
        match value {
            dashmap::mapref::entry::Entry::Occupied(e) => {
                if e.get().is_some() {
                    let out_data = e.remove().unwrap();
                    return Poll::Ready(out_data);
                }
            }
            dashmap::mapref::entry::Entry::Vacant(_) => {}
        }

        wake_mgt.wakers.push(cx.waker().clone());
        return Poll::Pending;
    }
}

#[derive(Default, Clone)]
pub struct WakerManager {
    pub wakers: Arc<SegQueue<Waker>>,
}

impl WakerManager {
    pub fn start(&self, duration_millis: u64) {
        let copied_manager = self.clone();
        tokio::spawn(async move {
            let manager = copied_manager;
            loop {
                // std::thread::sleep(dur);
                tokio::time::sleep(tokio::time::Duration::from_millis(duration_millis)).await;
                let queue_length = manager.wakers.len();
                for _i in 0..queue_length {
                    if let Some(item) = manager.wakers.pop() {
                        item.wake();
                    }
                }
            }
        });
    }
}
