use std::io::{ErrorKind, Read};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use crate::segment::TcpSegment;

pub const MAX_PAYLOAD: usize = 1 << 20;

#[derive(Debug)]
pub enum CaptureMessage {
    Segment(TcpSegment),
    Status(String),
    Stats { received: u32, dropped: u32, if_dropped: u32, segments: u64 },
}

fn read_ip(reader: &mut impl Read) -> std::io::Result<IpAddr> {
    let mut family = [0u8; 1];
    reader.read_exact(&mut family)?;
    let mut bytes = [0u8; 16];
    reader.read_exact(&mut bytes)?;
    let v6 = Ipv6Addr::from(bytes);
    Ok(match (family[0], v6.to_ipv4_mapped()) {
        (4, Some(v4)) => IpAddr::V4(v4),
        _ => IpAddr::V6(v6),
    })
}

fn read_u16(reader: &mut impl Read) -> std::io::Result<u16> {
    let mut b = [0u8; 2];
    reader.read_exact(&mut b)?;
    Ok(u16::from_le_bytes(b))
}

fn read_u32(reader: &mut impl Read) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    reader.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64(reader: &mut impl Read) -> std::io::Result<u64> {
    let mut b = [0u8; 8];
    reader.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

pub fn read_message(reader: &mut impl Read) -> std::io::Result<CaptureMessage> {
    let mut kind = [0u8; 1];
    reader.read_exact(&mut kind)?;
    match kind[0] {
        1 => {
            let timestamp = read_u64(reader)? as i64;
            let source_ip = read_ip(reader)?;
            let source_port = read_u16(reader)?;
            let destination_ip = read_ip(reader)?;
            let destination_port = read_u16(reader)?;
            let sequence = read_u32(reader)?;
            let mut flag = [0u8; 1];
            reader.read_exact(&mut flag)?;
            let length = read_u32(reader)? as usize;
            if length > MAX_PAYLOAD {
                return Err(std::io::Error::new(ErrorKind::InvalidData, "segment too long"));
            }
            let mut payload = vec![0u8; length];
            reader.read_exact(&mut payload)?;
            Ok(CaptureMessage::Segment(TcpSegment {
                timestamp,
                source: SocketAddr::new(source_ip, source_port),
                destination: SocketAddr::new(destination_ip, destination_port),
                sequence,
                payload,
                from_server: flag[0] != 0,
            }))
        }
        2 => {
            let length = read_u16(reader)? as usize;
            let mut text = vec![0u8; length];
            reader.read_exact(&mut text)?;
            Ok(CaptureMessage::Status(String::from_utf8_lossy(&text).into_owned()))
        }
        3 => Ok(CaptureMessage::Stats {
            received: read_u32(reader)?,
            dropped: read_u32(reader)?,
            if_dropped: read_u32(reader)?,
            segments: read_u64(reader)?,
        }),
        other => Err(std::io::Error::new(ErrorKind::InvalidData, format!("unknown message {other}"))),
    }
}
