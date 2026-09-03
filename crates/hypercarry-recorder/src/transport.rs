use futures_util::{SinkExt, StreamExt};
use hypercarry_core::info::Network;
use std::{error::Error, fmt, future::Future};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Error as WebSocketError, Message},
};

/// Transport-level message consumed by the recorder loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportMessage {
    /// A text data frame.
    Text(String),
    /// A ping control frame with its payload.
    Ping(Vec<u8>),
    /// A pong control frame.
    Pong,
    /// A close frame carrying an optional reason.
    Close(Option<String>),
}

/// One connected market-data stream.
pub trait MarketConnection: Send {
    /// Send a text frame, such as a subscription request.
    fn send_text(
        &mut self,
        text: String,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Reply to a server ping with a matching pong payload.
    fn send_pong(
        &mut self,
        payload: Vec<u8>,
    ) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Await the next transport message, or `None` at end of stream.
    fn next_message(
        &mut self,
    ) -> impl Future<Output = Result<Option<TransportMessage>, TransportError>> + Send;

    /// Close the connection at a clean boundary.
    fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send;
}

/// Reconnectable source of public market-data connections.
pub trait MarketTransport: Clone + Send + Sync + 'static {
    /// Connection type produced by this transport.
    type Connection: MarketConnection;

    /// Network this transport connects to.
    fn network(&self) -> Network;

    /// Establish one new connection.
    fn connect(&self) -> impl Future<Output = Result<Self::Connection, TransportError>> + Send;
}

/// Production transport using Hyperliquid's official network-specific URL.
#[derive(Debug, Clone, Copy)]
pub struct HyperliquidWebSocketTransport {
    network: Network,
}

impl HyperliquidWebSocketTransport {
    /// Build a transport bound to the official URL for `network`.
    pub const fn for_network(network: Network) -> Self {
        Self { network }
    }
}

/// A live Hyperliquid WebSocket connection.
pub struct HyperliquidConnection {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl MarketTransport for HyperliquidWebSocketTransport {
    type Connection = HyperliquidConnection;

    fn network(&self) -> Network {
        self.network
    }

    async fn connect(&self) -> Result<Self::Connection, TransportError> {
        let (stream, _) = connect_async(self.network.websocket_endpoint())
            .await
            .map_err(TransportError::WebSocket)?;
        Ok(HyperliquidConnection { stream })
    }
}

impl MarketConnection for HyperliquidConnection {
    async fn send_text(&mut self, text: String) -> Result<(), TransportError> {
        self.stream
            .send(Message::Text(text.into()))
            .await
            .map_err(TransportError::WebSocket)
    }

    async fn send_pong(&mut self, payload: Vec<u8>) -> Result<(), TransportError> {
        self.stream
            .send(Message::Pong(payload.into()))
            .await
            .map_err(TransportError::WebSocket)
    }

    async fn next_message(&mut self) -> Result<Option<TransportMessage>, TransportError> {
        loop {
            let Some(message) = self.stream.next().await else {
                return Ok(None);
            };
            let message = message.map_err(TransportError::WebSocket)?;
            let message = match message {
                Message::Text(text) => TransportMessage::Text(text.to_string()),
                Message::Binary(bytes) => TransportMessage::Text(
                    String::from_utf8(bytes.to_vec()).map_err(TransportError::BinaryUtf8)?,
                ),
                Message::Ping(payload) => TransportMessage::Ping(payload.to_vec()),
                Message::Pong(_) => TransportMessage::Pong,
                Message::Close(frame) => {
                    TransportMessage::Close(frame.map(|frame| frame.reason.to_string()))
                }
                Message::Frame(_) => continue,
            };
            return Ok(Some(message));
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.stream
            .close(None)
            .await
            .map_err(TransportError::WebSocket)
    }
}

/// WebSocket connection or frame failure.
#[derive(Debug)]
pub enum TransportError {
    /// The underlying WebSocket connection failed.
    WebSocket(WebSocketError),
    /// A binary frame did not decode as UTF-8 text.
    BinaryUtf8(std::string::FromUtf8Error),
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WebSocket(error) => write!(formatter, "WebSocket transport failed: {error}"),
            Self::BinaryUtf8(error) => {
                write!(formatter, "WebSocket binary frame is not UTF-8: {error}")
            }
        }
    }
}

impl Error for TransportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::WebSocket(error) => Some(error),
            Self::BinaryUtf8(error) => Some(error),
        }
    }
}
