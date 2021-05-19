use std::net::ToSocketAddrs;
use std::io;
use async_net::TcpStream;
use crate::session::Session;
use crate::connection::{OwnedCredentials, HeartBeat};
use crate::header::HeaderList;
use crate::option_setter::OptionSetter;
use asynchronous_codec::Framed;
use crate::codec::Codec;
use crate::errors::Error;

#[derive(Clone)]
pub struct SessionConfig {
    pub host: String,
    pub port: u16,
    pub credentials: Option<OwnedCredentials>,
    pub heartbeat: HeartBeat,
    pub headers: HeaderList,
}

pub struct SessionBuilder {
    pub config: SessionConfig
}

impl SessionBuilder {
    pub fn new(host: &str,
               port: u16)
               -> SessionBuilder {
        let config = SessionConfig {
            host: host.to_owned(),
            port: port,
            credentials: None,
            heartbeat: HeartBeat(0, 0),
            headers: header_list![
           //"host" => host,
           "accept-version" => "1.2",
           "content-length" => "0"
          ],
        };
        SessionBuilder {
            config: config,
        }
    }

    #[allow(dead_code)]
    pub async fn start<'b, 'c>(self) -> Result<Session, Error> {
        let address = (&self.config.host as &str, self.config.port)
            .to_socket_addrs()
            .map_err(Error::Io)?
            .nth(0)
            .ok_or(Error::Io(io::Error::new(io::ErrorKind::Other, "address provided resolved to nothing")))?;
        let tcp_stream = TcpStream::connect(&address)
            .await
            .map_err(Error::Io)?;

        let tcp_stream = Framed::new(tcp_stream, Codec);
        debug!("connected...");
        Session::new(self.config, tcp_stream).await
    }

    #[allow(dead_code)]
    pub fn with<'b, T>(self, option_setter: T) -> SessionBuilder
        where T: OptionSetter<SessionBuilder>
    {
        option_setter.set_option(self)
    }
}
