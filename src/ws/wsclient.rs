use super::super::{ClientCallback, ConnectorError, DataEntity};
use futures_util::{future, pin_mut, StreamExt};
use std::sync::{Arc, Mutex};
use std::{
    io::{Error, ErrorKind},
    time::Duration,
};
use tracing::*;

use tokio::{net::TcpStream, time::timeout as tout};
#[allow(unused_imports)]
use tokio_tungstenite::{
    connect_async, connect_async_with_config,
    tungstenite::{client, client::IntoClientRequest, protocol::Message},
};

pub fn parse_timeout(timeout: u64) -> Result<Duration, Error> {
    if timeout == 0 {
        // To be consistent with the async implementation:
        // https://github.com/rust-lang/rust/blob/e51830b90afd339332892a8f20db1957d43bf086/library/std/src/sys/unix/net.rs#L142
        Err(Error::new(
            ErrorKind::InvalidInput,
            "cannot set a 0 duration timeout",
        ))
    } else {
        Ok(Duration::from_secs(timeout))
    }
}

async fn connect_timeout(addrs: &str, dur: Duration) -> Result<(), Error> {
    match tout(dur, TcpStream::connect(addrs)).await {
        Ok(_) => Ok(()),
        Err(e) => Err(Error::from(e)),
    }
}

pub const ADDRS: [&str; 2] = ["baidu.com:80", "bing.com:80"];

pub async fn check(timeout: Option<u64>) -> Result<(), Error> {
    // Avoiding `io:timeout` in this case to allow the OS decide for
    // better diagnostics.
    if let Some(t) = timeout {
        let dur = parse_timeout(t)?;
        // First try, ignoring error (if any).
        return if connect_timeout(ADDRS[0], dur).await.is_ok() {
            Ok(())
        } else {
            // Fallback.
            connect_timeout(ADDRS[1], dur).await
        };
    }

    // No timeout.
    if TcpStream::connect(ADDRS[0]).await.is_ok() {
        Ok(())
    } else if let Err(e) = TcpStream::connect(ADDRS[1]).await {
        Err(e)
    } else {
        Ok(())
    }
}

#[allow(dead_code)]
#[derive(Clone)]
pub struct WsClient {
    pub url: String,                       // websocket connect url
    pub reconnect: bool,                   // true-reconnect always false-not reconnect
    pub callback: Arc<dyn ClientCallback>, // data handler
    pub token: Option<String>,             // web token
    pub reconn_time: u64,                  // reconnect time, default is 1 second

    pub tx: Arc<Mutex<Option<futures_channel::mpsc::UnboundedSender<Message>>>>,
    pub runtime: Arc<Mutex<Option<tokio::runtime::Runtime>>>,
    pub handle: tokio::runtime::Handle, // tokio handler
    pub conn_join: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    pub conn_status: Arc<Mutex<bool>>,

    pub probe_thread_status: Arc<Mutex<bool>>, // state of web probe
}

impl WsClient {
    pub fn get_runtime_handler() -> (tokio::runtime::Handle, Option<tokio::runtime::Runtime>) {
        // if current's env don't have tokio runtime, then create one new runtime
        match tokio::runtime::Handle::try_current() {
            Ok(h) => (h, None),
            Err(_) => {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .thread_name("connector")
                    .enable_all()
                    .build()
                    .unwrap();
                (rt.handle().clone(), Some(rt))
            }
        }
    }

    pub fn new(
        url: &str,
        token: Option<String>,
        callback: Arc<dyn ClientCallback>,
        reconnect: bool,
        reconn_time: Option<u64>,
    ) -> Self {
        let (handle, rt) = Self::get_runtime_handler();
        let reconn_time = if reconn_time.is_none() {
            1
        } else {
            reconn_time.unwrap()
        };

        Self {
            url: url.to_string(),
            reconnect: reconnect,
            callback: callback,
            token: token,
            reconn_time: reconn_time,

            tx: Arc::new(Mutex::new(None)),
            runtime: Arc::new(Mutex::new(rt)),
            handle: handle,
            conn_join: Arc::new(Mutex::new(None)),
            conn_status: Arc::new(Mutex::new(false)),
            probe_thread_status: Arc::new(Mutex::new(false)),
        }
    }

    pub fn send_binary(&self, data: Vec<u8>) -> anyhow::Result<()> {
        if *self.conn_status.lock().unwrap() {
            if let Some(tx) = self.tx.lock().unwrap().clone() {
                tx.unbounded_send(Message::Binary(data))?;
                return anyhow::Ok(());
            } else {
                warn!("[connector]send data with error as tx is none, not valid");
                return Err(anyhow::anyhow!(
                    "[connector]send data with error as tx is none, not valid"
                ));
            }
        } else {
            warn!("[connector]send data with error as conn_status is false");
            return Err(anyhow::anyhow!(
                "[connector]send data with error as conn_status is false"
            ));
        }
    }

    pub fn send_text(&self, data: String) -> anyhow::Result<()> {
        if *self.conn_status.lock().unwrap() {
            if let Some(tx) = self.tx.lock().unwrap().clone() {
                tx.unbounded_send(Message::Text(data))?;
                return anyhow::Ok(());
            } else {
                warn!("[connector]send data with error as tx is none, not valid");
                return Err(anyhow::anyhow!(
                    "[connector]send data with error as tx is none, not valid"
                ));
            }
        } else {
            warn!("[connector]send data with error as conn_status is false");
            return Err(anyhow::anyhow!(
                "[connector]send data with error as conn_status is false"
            ));
        }
    }

    pub fn start_network_probe(&self) {
        let client = self.clone();

        // only in reconnect mode, then start web probe thread
        if client.reconnect {
            self.handle.spawn(async move {
                let mut client = client;
                *client.probe_thread_status.lock().unwrap() = true;

                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(20)).await;
                    let status = check(None).await;
                    match status {
                        Ok(_) => {
                            if client.conn_join.lock().unwrap().as_mut().is_none() {
                                warn!("network is available and start websockt thread again");
                                client.start();
                            }
                        }
                        Err(_e) => {
                            warn!("network is not available: {:?}", _e);
                            let mut conn_join = client.conn_join.lock().unwrap();
                            if let Some(conn_task) = conn_join.as_mut() {
                                warn!("network is not available and abort websockt thread");
                                conn_task.abort();
                                drop(conn_join);
                                *client.conn_join.lock().unwrap() = None;
                            }
                        }
                    }
                }
                // if thread is stop, set probe_thread_status as false
                // *client.probe_thread_status.lock().unwrap() = false;
            });
        }
    }

    pub fn start_monitor(&self) {
        let client = self.clone();
        self.handle.spawn(async move {
            let mut client = client;
            // info!("[monitor]ws client start monitor thread");

            // wait some second, about the thread and then restart it
            // std::thread::sleep(std::time::Duration::from_secs(1));
            // std::thread::sleep(std::time::Duration::from_secs(client.reconn_time));
            tokio::time::sleep(tokio::time::Duration::from_secs(client.reconn_time)).await;
            if let Some(conn_task) = client.conn_join.lock().unwrap().as_mut() {
                conn_task.abort();
            }
            *client.conn_join.lock().unwrap() = None;

            trace!("[monitor]restart client connecting thread");
            client.start();
        });
    }

    fn callback_entry(&self, data: DataEntity) {
        match data {
            DataEntity::Error { err: _ } => {
                *self.conn_status.lock().unwrap() = false;
                if self.reconnect {
                    self.start_monitor();
                }
            }
            _ => {}
        }
        // futures::executor::block_on(async {
        //     self.callback
        //         .process(data, self.callback.clone(), self)
        //         .await;
        // });

        let client = self.clone();
        tokio::spawn(async move {
            let client = client;
            client
                .callback
                .process(data, client.callback.clone(), &client)
                .await;
        });
    }

    pub fn start(&mut self) {
        let url = self.url.clone();
        let url_obj = http::Uri::from_static(Box::leak(url.into_boxed_str()));
        let (stdin_tx, stdin_rx) = futures_channel::mpsc::unbounded();
        *self.tx.lock().unwrap() = Some(stdin_tx.clone());

        let func = |url: http::Uri,
                    stdin_rx: futures_channel::mpsc::UnboundedReceiver<Message>,
                    client: Self| async move {
            // info!("url: {:?}", url);
            {
                *client.conn_status.lock().unwrap() = true;
                // info!("[listener]ws client connect thread starting");
            }

            // config Header and other info
            let request = if client.token.clone().is_none() {
                url.into_client_request().unwrap()
            } else {
                let mut request = url.into_client_request().unwrap();
                let token = client.token.clone().unwrap();
                request
                    .headers_mut()
                    .append("token", token.parse().unwrap());
                request
            };

            // start websocket connection
            // let tmp_conn = connect_async(url).await;
            // info!("{:?}", request);
            // let tmp_conn = connect_async(request).await;
            let tmp_conn = connect_async_with_config(request, None, false).await;
            if tmp_conn.is_err() {
                let error_info = tmp_conn.as_ref().err().unwrap();
                // for (key, value) in std::env::vars() {
                //     error!("[listener]{key}: {value}");
                // }
                // error!("[listener]error: {}", error_info);
                client.callback_entry(DataEntity::Error {
                    err: ConnectorError::WsConnectError(format!(
                        "ws connect error: {:?}",
                        error_info
                    )),
                });
                return;
            }
            let (ws_stream, _resp) = tmp_conn.unwrap();
            // send connect successfully info and check wheth or not start the monitor thread
            client.callback_entry(DataEntity::Success);

            let (write, read) = ws_stream.split();
            let stdin_to_ws = stdin_rx.map(Ok).forward(write);

            // sometimes DNS ping is not successfully, it will let the normal task will be aborted, ignre is first
            // if !*client.probe_thread_status.lock().unwrap() {
            //     client.start_network_probe();
            // }
            let ws_to_stdout = {
                read.for_each(|message| async {
                    match message {
                        Ok(message) => {
                            if message.is_text() {
                                let data = message.to_text().unwrap();
                                client.callback_entry(DataEntity::Text {
                                    data: data.to_string(),
                                });
                            } else if message.is_binary() {
                                let data = message.into_data();
                                client.callback_entry(DataEntity::Binary { data: data });
                            } else if message.is_ping() {
                                let data = message.into_data();
                                let _ = client
                                    .tx
                                    .lock()
                                    .unwrap()
                                    .as_mut()
                                    .unwrap()
                                    .unbounded_send(Message::Pong(data));
                            } else if message.is_close() {
                                warn!("client websocket is cloesd");
                            }
                        }
                        Err(e) => {
                            warn!("client websocket recved error in for_each");
                            client.callback_entry(DataEntity::Error {
                                err: ConnectorError::WsReadError(format!(
                                    "ws read data error:  {:?}",
                                    e
                                )),
                            });
                        }
                    }
                })
            };
            pin_mut!(stdin_to_ws, ws_to_stdout);
            future::select(stdin_to_ws, ws_to_stdout).await;
            error!("[wsclient] recv thread exit")
        };

        // save thread handler
        let conn_thread = self.handle.spawn(func(url_obj, stdin_rx, self.clone()));
        *self.conn_join.lock().unwrap() = Some(conn_thread);
    }
}
