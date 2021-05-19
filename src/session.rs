use std::collections::hash_map::HashMap;
use std::io::{Result, ErrorKind};
use futures::*;
use async_net::TcpStream;
use futures::task::{Poll, Context};
use std::pin::Pin;
use smol::Timer;
use asynchronous_codec::Framed;
use futures::future::FutureExt;
use futures::future::Future;
use futures::sink::SinkExt;
use crate::frame::Transmission::{HeartBeat, CompleteFrame};
use crate::frame::{Frame, Transmission, ToFrameBody, Command};
use crate::message_builder::MessageBuilder ;
use crate::subscription_builder::SubscriptionBuilder;
use crate::connection::Connection;
use crate::subscription::{Subscription, AckMode, AckOrNack};
use crate::codec::Codec;
use crate::session_builder::SessionConfig;
use crate::header::Header;
use smol::stream::StreamExt;


const GRACE_PERIOD_MULTIPLIER: f32 = 2.0;

pub struct OutstandingReceipt {
    pub original_frame: Frame,
}

impl OutstandingReceipt {
    pub fn new(original_frame: Frame) -> Self {
        OutstandingReceipt {
            original_frame: original_frame
        }
    }
}
pub struct GenerateReceipt;
pub struct ReceiptRequest {
    pub id: String,
}

impl ReceiptRequest {
    pub fn new(id: String) -> Self {
        ReceiptRequest {
            id: id,
        }
    }
}

pub struct SessionState {
    next_transaction_id: u32,
    next_subscription_id: u32,
    next_receipt_id: u32,
    pub rx_heartbeat_ms: Option<u32>,
    pub tx_heartbeat_ms: Option<u32>,
    pub rx_heartbeat_timeout: Option<Timer>,
    pub tx_heartbeat_timeout: Option<Timer>,
    pub subscriptions: HashMap<String, Subscription>,
    pub outstanding_receipts: HashMap<String, OutstandingReceipt>
}

impl SessionState {
    pub fn new() -> SessionState {
        SessionState {
            next_transaction_id: 0,
            next_subscription_id: 0,
            next_receipt_id: 0,
            rx_heartbeat_ms: None,
            rx_heartbeat_timeout: None,
            tx_heartbeat_ms: None,
            tx_heartbeat_timeout: None,
            subscriptions: HashMap::new(),
            outstanding_receipts: HashMap::new(),
        }
    }
}

// *** Public API ***
impl Session {
    pub async fn send_frame(&mut self, fr: Frame) {
        self.send(Transmission::CompleteFrame(fr)).await
    }
    pub fn message<T: ToFrameBody>(&mut self,
                                   destination: &str,
                                   body_convertible: T)
                                   -> MessageBuilder {
        let send_frame = Frame::send(destination, body_convertible.to_frame_body());
        MessageBuilder::new(self, send_frame)
    }

    pub fn subscription(&mut self,
                        destination: &str)
                        -> SubscriptionBuilder
    {
        SubscriptionBuilder::new(self, destination.to_owned())
    }

    // pub fn begin_transaction<'b>(&'b mut self) -> Transaction<'b> {
    //     let mut transaction = Transaction::new(self);
    //     let _ = transaction.begin();
    //     transaction
    // }

    pub async fn unsubscribe(&mut self, sub_id: &str) {
        self.state.subscriptions.remove(sub_id);
        let unsubscribe_frame = Frame::unsubscribe(sub_id.as_ref());
        self.send(CompleteFrame(unsubscribe_frame)).await
    }

    pub async fn disconnect(&mut self) {
        self.send_frame(Frame::disconnect()).await;
    }
    // pub fn reconnect(&mut self) -> ::std::io::Result<()> {
    //     use std::net::ToSocketAddrs;
    //     use std::io;
    //
    //     info!("Reconnecting...");
    //
    //     let address = (&self.config.host as &str, self.config.port)
    //         .to_socket_addrs()?.nth(0)
    //         .ok_or(io::Error::new(io::ErrorKind::Other, "address provided resolved to nothing"))?;
    //     self.stream = StreamState::Connecting(TcpStream::connect(&address));
    //     task::current().notify();
    //     Ok(())
    // }
    pub async fn acknowledge_frame(&mut self, frame: &Frame, which: AckOrNack) {
        if let Some(crate::header::Ack(ack_id)) = frame.headers.get_ack() {
            let ack_frame = if let AckOrNack::Ack = which {
                Frame::ack(ack_id)
            }
            else {
                Frame::nack(ack_id)
            };
            self.send_frame(ack_frame).await;
        }
    }
}
// *** pub(crate) API ***
impl Session {
    pub(crate) async fn new(config: SessionConfig, stream: Framed<TcpStream, Codec>) -> Self {
        let mut session = Self {
            config,
            state: SessionState::new(),
            //events: vec![],
            stream: StreamState::Connected(stream),
        };

        session.do_connect().await;
        session
    }
    pub(crate) fn generate_transaction_id(&mut self) -> u32 {
        let id = self.state.next_transaction_id;
        self.state.next_transaction_id += 1;
        id
    }

    pub(crate) fn generate_subscription_id(&mut self) -> u32 {
        let id = self.state.next_subscription_id;
        self.state.next_subscription_id += 1;
        id
    }

    pub(crate) fn generate_receipt_id(&mut self) -> u32 {
        let id = self.state.next_receipt_id;
        self.state.next_receipt_id += 1;
        id
    }
}
// *** Internal API ***
impl Session {
    async fn _send(&mut self, tx: Transmission) -> Result<()> {
        if let StreamState::Connected(ref mut st) = self.stream {
            debug!("Stream is sending");
            st.send(tx).await?;
            debug!("Stream is done sending");
        }
        else {
            warn!("sending {:?} whilst disconnected", tx);
        }
        Ok(())
    }
    async fn send(&mut self, tx: Transmission) {
        debug!("send() {:?}", tx);
        if let Err(e) = self._send(tx).await {
            self.on_disconnect(DisconnectionReason::SendFailed(e));
        }
        debug!("send() end");
    }
    fn register_tx_heartbeat_timeout(&mut self) -> Result<()> {
        use std::time::Duration;
        if self.state.tx_heartbeat_ms.is_none() {
            warn!("Trying to register TX heartbeat timeout, but not set!");
            return Ok(());
        }
        let tx_heartbeat_ms = self.state.tx_heartbeat_ms.unwrap(); //TODO return result
        if tx_heartbeat_ms <= 0 {
            debug!("Heartbeat transmission ms is {}, no need to register a callback.",
                   tx_heartbeat_ms);
            return Ok(());
        }
        let timeout = Timer::after(Duration::from_millis(tx_heartbeat_ms as _));
        self.state.tx_heartbeat_timeout = Some(timeout);
        Ok(())
    }

    fn register_rx_heartbeat_timeout(&mut self) -> Result<()> {
        use std::time::Duration;

        let rx_heartbeat_ms = self.state.rx_heartbeat_ms
            .unwrap_or_else(|| {
                debug!("Trying to register RX heartbeat timeout but no \
                        rx_heartbeat_ms was set. This is expected for receipt \
                        of CONNECTED.");
                0
            });
        if rx_heartbeat_ms <= 0 {
            debug!("Heartbeat receipt ms is {}, no need to register a callback.",
                   rx_heartbeat_ms);
            return Ok(());
        }
        let timeout = Timer::after(Duration::from_millis(rx_heartbeat_ms as _));
        self.state.rx_heartbeat_timeout = Some(timeout);
        Ok(())
    }

    fn on_recv_data(&mut self) -> Result<()> {
        if self.state.rx_heartbeat_ms.is_some() {
            self.register_rx_heartbeat_timeout()?;
        }
        Ok(())
    }

    async fn reply_to_heartbeat(&mut self) -> Result<()> {
        debug!("Sending heartbeat");
        self.send(HeartBeat).await;
        self.register_tx_heartbeat_timeout()?;
        Ok(())
    }

    fn on_disconnect(&mut self, reason: DisconnectionReason) {
        info!("Disconnected. Reason: {:?}", reason);
        //self.events.push(SessionEvent::Disconnected(reason));
        if let StreamState::Connected(ref mut strm) = self.stream {
            let _ = strm.shutdown(::std::net::Shutdown::Both); //TODO handle result?
        }
        self.stream = StreamState::Failed;
        self.state.tx_heartbeat_timeout = None;
        self.state.rx_heartbeat_timeout = None;
    }

    async fn on_stream_ready(&mut self) {
        debug!("Stream ready!");
        // Add credentials to the header list if specified
        match self.config.credentials.clone() { // TODO: Refactor to avoid clone
            Some(credentials) => {
                debug!("Using provided credentials: login '{}', passcode '{}'",
                       credentials.login,
                       credentials.passcode);
                let mut headers = &mut self.config.headers;
                headers.push(Header::new("login", &credentials.login));
                headers.push(Header::new("passcode", &credentials.passcode));
            }
            None => debug!("No credentials supplied."),
        }

        let crate::connection::HeartBeat(client_tx_ms, client_rx_ms) = self.config.heartbeat;
        let heart_beat_string = format!("{},{}", client_tx_ms, client_rx_ms);
        debug!("Using heartbeat: {},{}", client_tx_ms, client_rx_ms);
        self.config.headers.push(Header::new("heart-beat", heart_beat_string.as_ref()));

        let connect_frame = Frame {
            command: Command::Connect,
            headers: self.config.headers.clone(), /* Cloned to allow this to be re-used */
            body: Vec::new(),
        };

        self.send_frame(connect_frame).await;
    }
    fn on_message(&mut self, frame: Frame) -> SessionEvent {
        let mut sub_data = None;
        if let Some(crate::header::Subscription(sub_id)) = frame.headers.get_subscription() {
            if let Some(ref sub) = self.state.subscriptions.get(sub_id) {
                sub_data = Some((sub.destination.clone(), sub.ack_mode));
            }
        }
        if let Some((destination, ack_mode)) = sub_data {
            SessionEvent::Message {
                destination,
                ack_mode,
                frame
            }
        }
        else {
            SessionEvent::SubscriptionlessFrame(frame)
        }
    }

    async fn do_connect(&mut self) {
        let connect = Frame::connect(self.config.heartbeat.0, self.config.heartbeat.1);
        self.send_frame(connect).await;
        self.run_stream().await;
    }

    fn on_connected_frame_received(&mut self, connected_frame: Frame) -> Result<SessionEvent> {
        debug!("Server confirms connection!");
        // The Client's requested tx/rx HeartBeat timeouts
        let crate::connection::HeartBeat(client_tx_ms, client_rx_ms) = self.config.heartbeat;

        // The timeouts the server is willing to provide
        let (server_tx_ms, server_rx_ms) = match connected_frame.headers.get_heart_beat() {
            Some(crate::header::HeartBeat(tx_ms, rx_ms)) => (tx_ms, rx_ms),
            None => (0, 0),
        };

        let (agreed_upon_tx_ms, agreed_upon_rx_ms) = Connection::select_heartbeat(client_tx_ms,
                                                                                  client_rx_ms,
                                                                                  server_tx_ms,
                                                                                  server_rx_ms);
        self.state.rx_heartbeat_ms = Some((agreed_upon_rx_ms as f32 * GRACE_PERIOD_MULTIPLIER) as u32);
        self.state.tx_heartbeat_ms = Some(agreed_upon_tx_ms);

        self.register_tx_heartbeat_timeout()?;
        self.register_rx_heartbeat_timeout()?;


        Ok(SessionEvent::Connected)
    }
    fn handle_receipt(&mut self, frame: Frame) -> Option<SessionEvent> {
        let receipt_id = {
            if let Some(crate::header::ReceiptId(receipt_id)) = frame.headers.get_receipt_id() {
                Some(receipt_id.to_owned())
            }
            else {
                None
            }
        };
        if let Some(receipt_id) = receipt_id {
            if receipt_id == "msg/disconnect" {
                self.on_disconnect(DisconnectionReason::Requested);
            }
            if let Some(entry) = self.state.outstanding_receipts.remove(&receipt_id) {
                let original_frame = entry.original_frame;
                return Some(SessionEvent::Receipt {
                    id: receipt_id,
                    original: original_frame,
                    receipt: frame
                });
            }
        }

        None
    }

    async fn poll_stream_complete(&mut self) {
        let res = {
            if let StreamState::Connected(ref mut fr) = self.stream {
                debug!("Attempt flush");
                fr.flush().await // https://docs.rs/tokio-io/0.1.2/tokio_io/codec/struct.Framed.html#method.poll_complete
            }
            else {
                Err(std::io::Error::new(ErrorKind::NotConnected, std::io::Error::from(ErrorKind::NotConnected)))
            }
        };
        if let Err(e) = res {
            self.on_disconnect(DisconnectionReason::SendFailed(e));
        }
    }
    async fn poll_stream(&mut self) -> Result<Option<Transmission>> {
        use self::StreamState::*;
        match &mut self.stream {
            Connected(fr) => {
                println!("connected ");
                match fr.next().await {
                    Some(Ok(r)) => {
                        debug!("poll_stream got {:?}", r);
                        return Ok(Some(r));
                    },
                    None => {
                        debug!("Got None");
                        self.on_disconnect(DisconnectionReason::ClosedByOtherSide);
                        return Ok(None);
                    },
                    Some(Err(e)) => {
                        debug!("Got Error {:?}", e);
                        let ret = Err(std::io::Error::from(e.kind()));
                        self.on_disconnect(DisconnectionReason::RecvFailed(e));
                        return ret;
                    },
                }
            },
            Failed => {
                println!("error");
                return Err(std::io::Error::new(ErrorKind::BrokenPipe, std::io::Error::from(ErrorKind::BrokenPipe)));
            },
        }
    }

    async fn run_stream(&mut self) -> Result<Option<SessionEvent>> {
        use crate::frame::Transmission::*;
        println!("Called");

        match self.poll_stream().await {
            Ok(Some(val)) => {
                let event = match val {
                    HeartBeat => {
                        debug!("Received heartbeat.");
                        self.on_recv_data()?;
                        None
                    },
                    CompleteFrame(frame) => {
                        debug!("Received frame: {:?}", frame);
                        self.on_recv_data()?;
                        match frame.command {
                            Command::Error => Some(SessionEvent::ErrorFrame(frame)),
                            Command::Receipt => self.handle_receipt(frame),
                            Command::Connected => Some(self.on_connected_frame_received(frame)?),
                            Command::Message => Some(self.on_message(frame)),
                            _ => Some(SessionEvent::UnknownFrame(frame))
                        }
                    }
                };

                if let Some(ref mut timer) = self.state.rx_heartbeat_timeout {
                    if let Poll::Ready(_) = poll!(timer) {
                        self.on_disconnect(DisconnectionReason::HeartbeatTimeout);
                    }
                }

                if let Some(ref mut timer) = self.state.tx_heartbeat_timeout {
                    if let Poll::Ready(_) = poll!(timer) {
                        self.reply_to_heartbeat().await?;
                    }
                }

                self.poll_stream_complete().await;

                return Ok(event);
            },
            Ok(None) => {
                return Ok(None);
            },
            Err(e) => {
                return Err(std::io::Error::from(e.kind()))
            }
        }
    }
}
#[derive(Debug)]
pub enum DisconnectionReason {
    RecvFailed(::std::io::Error),
    ConnectFailed(::std::io::Error),
    SendFailed(::std::io::Error),
    ClosedByOtherSide,
    HeartbeatTimeout,
    Requested
}
#[derive(Debug)]
pub enum SessionEvent {
    Connected,
    ErrorFrame(Frame),
    Receipt {
        id: String,
        original: Frame,
        receipt: Frame
    },
    Message {
        destination: String,
        ack_mode: AckMode,
        frame: Frame
    },
    SubscriptionlessFrame(Frame),
    UnknownFrame(Frame),
    Disconnected(DisconnectionReason)
}
pub(crate) enum StreamState {
    Connected(Framed<TcpStream, Codec>),
    Failed
}
pub struct Session {
    config: SessionConfig,
    pub(crate) state: SessionState,
    stream: StreamState,
}


impl Stream for Session {
    type Item = Result<SessionEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let mut next = self.run_stream().boxed();
            let next = Pin::new(&mut next).poll(cx);
            debug!("next: {:?}", next);
            match next {
                Poll::Pending => {
                    return Poll::Pending;
                },
                Poll::Ready(Err(e)) => {
                    return Poll::Ready(Some(Err(e)));
                },
                Poll::Ready(Ok(None)) => {
                    //return Poll::Ready(Some(Ok(None)));
                },
                Poll::Ready(Ok(Some(event))) => {
                    return Poll::Ready(Some(Ok(event)));
                },
            }
        }
    }
}
