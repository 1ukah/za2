use std::net::SocketAddr;

pub type Micros = i64;

#[derive(Clone, Debug)]
pub struct TcpSegment {
    pub timestamp: Micros,
    pub source: SocketAddr,
    pub destination: SocketAddr,
    pub sequence: u32,
    pub payload: Vec<u8>,
    pub from_server: bool,
}

impl TcpSegment {
    pub fn stream_key(&self) -> (SocketAddr, SocketAddr) {
        (self.source, self.destination)
    }
}

pub fn now_micros() -> Micros {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

pub const SECOND: Micros = 1_000_000;
