use webclient::command::{ClientCommand, WakerManager};
use webclient::*;
use webproto;

use async_trait::async_trait;
use dashmap::DashMap;
use std::any::Any;
use std::sync::Arc;
use time::{macros::format_description, UtcOffset};
use tracing::*;
use tracing_subscriber::{fmt, prelude::*};

type TargetMessage = webproto::MockMessage;

#[derive(Clone)]
struct DataHandler<T: Send + Sync + serde::de::DeserializeOwned + 'static> {
    pub queue: Arc<DashMap<String, Option<T>>>,
    pub wake_mgt: WakerManager,
}

impl<T: Send + Sync + serde::de::DeserializeOwned + 'static> DataHandler<T> {
    pub fn new() -> Self {
        let queue = Arc::new(DashMap::<String, Option<T>>::new());
        let wake_mgt = WakerManager::default();
        Self {
            queue: queue,
            wake_mgt: wake_mgt,
        }
    }
}

// #[async_trait(?Send)]
#[async_trait]
#[allow(unused_variables)]
impl<T: Send + Sync + serde::de::DeserializeOwned + 'static> ClientCallback for DataHandler<T> {
    async fn process(
        &self,
        data: DataEntity,
        callback: Arc<dyn ClientCallback>,
        client: &WsClient,
    ) {
        match data {
            DataEntity::Binary { data } => {
                // let bin_data = data.as_bytes();
                // let msg = webproto::Message::from_vec(&bin_data.to_vec()).unwrap();
                let msg = webproto::decode_message::<T>(&data).unwrap();
                match msg {
                    webproto::Message::ClientCommand(command) => {
                        info!("recv client command response");
                        info!("recv command msg");
                        let event_id = command.event_id.clone();
                        self.queue
                            .entry(event_id)
                            .and_modify(|v| *v = Some(command.command));
                    }
                    webproto::Message::Indication(indication) => {}
                    webproto::Message::ServerCommand(command) => {
                        info!("recv server command request");
                        let event_id = command.event_id.clone();
                        let resp_msg = TargetMessage::mock_response();
                        let _resp_cmd = ClientCommand::<TargetMessage>::send_answer(
                            &client, resp_msg, event_id,
                        )
                        .await;
                    }
                }
            }
            _ => {
                info!("other type data {:?}", data);
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let local_time = tracing_subscriber::fmt::time::OffsetTime::new(
        UtcOffset::from_hms(8, 0, 0).unwrap(),
        format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:6]"),
    );
    let (stdoutlog, _guard) = tracing_appender::non_blocking(std::io::stdout());
    let subscriber = tracing_subscriber::registry().with(
        fmt::Layer::new()
            .with_writer(stdoutlog.with_max_level(tracing::Level::INFO))
            .with_timer(local_time.clone())
            // .without_time()
            .with_ansi(false)
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .with_thread_ids(true)
            .with_level(true), // .pretty(),
    );
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let handler = Arc::new(DataHandler::<TargetMessage>::new());
    handler.wake_mgt.start(1);

    let mut client = WsClient::new(
        "ws://localhost:9000/ws/api/test/mine/master",
        None,
        handler.clone(),
        true,
        None,
    );
    client.start();

    // loop {
    //     std::thread::sleep(std::time::Duration::from_secs(1));
    //     let msg = TargetMessage::mock_request();
    //     let rev_msg = ClientCommand::<TargetMessage>::send_command(
    //         &client,
    //         msg,
    //         1,
    //         &handler.queue,
    //         &handler.wake_mgt,
    //     )
    //     .await;
    //     tracing::info!("{:?}", rev_msg);
    //     tracing::info!("{:?}", handler.queue);
    // }

    use std::sync::mpsc::channel;
    let (tx, rx) = channel();
    ctrlc::set_handler(move || tx.send(()).expect("Could not send signal on channel."))
        .expect("Error setting Ctrl-C handler");
    rx.recv().expect("Could not receive from channel.");
    println!("Ctrl-C and exiting...");
    std::process::exit(0);
    // anyhow::Ok(())
}
