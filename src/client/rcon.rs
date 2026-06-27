//! Source RCON protocol client for Factorio
//!
//! Implementation based on: https://developer.valvesoftware.com/wiki/Source_RCON_Protocol

use anyhow::{bail, Context, Result};
use std::sync::atomic::{AtomicI32, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};

/// RCON packet types
const SERVERDATA_AUTH: i32 = 3;
const SERVERDATA_AUTH_RESPONSE: i32 = 2;
const SERVERDATA_EXECCOMMAND: i32 = 2;
const SERVERDATA_RESPONSE_VALUE: i32 = 0;

/// Default timeout for RCON operations
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// An RCON packet
#[derive(Debug, Clone)]
pub struct RconPacket {
    pub request_id: i32,
    pub packet_type: i32,
    pub body: String,
}

impl RconPacket {
    /// Create a new packet
    pub fn new(request_id: i32, packet_type: i32, body: impl Into<String>) -> Self {
        Self {
            request_id,
            packet_type,
            body: body.into(),
        }
    }

    /// Encode packet to bytes for sending
    pub fn encode(&self) -> Vec<u8> {
        let body_bytes = self.body.as_bytes();
        // size = request_id (4) + type (4) + body + null (1) + null (1)
        let size = 4 + 4 + body_bytes.len() + 1 + 1;

        let mut buf = Vec::with_capacity(4 + size);

        // Size (little-endian i32)
        buf.extend_from_slice(&(size as i32).to_le_bytes());
        // Request ID (little-endian i32)
        buf.extend_from_slice(&self.request_id.to_le_bytes());
        // Packet type (little-endian i32)
        buf.extend_from_slice(&self.packet_type.to_le_bytes());
        // Body
        buf.extend_from_slice(body_bytes);
        // Null terminators
        buf.push(0);
        buf.push(0);

        buf
    }

    /// Decode packet from bytes (excluding size prefix)
    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < 10 {
            bail!("Packet too small: {} bytes", data.len());
        }

        let request_id = i32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let packet_type = i32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        // Body is everything after header, minus two null terminators
        let body_end = data.len().saturating_sub(2);
        let body = String::from_utf8_lossy(&data[8..body_end]).to_string();

        Ok(Self {
            request_id,
            packet_type,
            body,
        })
    }
}

/// RCON client for communicating with Factorio servers
pub struct RconClient {
    stream: Option<TcpStream>,
    request_id: AtomicI32,
}

impl RconClient {
    /// Connect and authenticate with an RCON server
    pub async fn connect(host: &str, port: u16, password: &str) -> Result<Self> {
        let addr = format!("{}:{}", host, port);
        let stream = timeout(DEFAULT_TIMEOUT, TcpStream::connect(&addr))
            .await
            .context("Connection timed out")?
            .context("Failed to connect")?;

        let mut client = Self {
            stream: Some(stream),
            request_id: AtomicI32::new(0),
        };

        // Authenticate
        let auth_id = client.next_id();
        let auth_packet = RconPacket::new(auth_id, SERVERDATA_AUTH, password);
        client.send(&auth_packet).await?;

        loop {
            let response = client.receive().await?;
            if auth_response_complete(&response, auth_id)? {
                break;
            }
        }

        Ok(client)
    }

    /// Execute a command and return the response
    pub async fn execute(&mut self, command: &str) -> Result<String> {
        let request_id = self.next_id();
        let sentinel_id = self.next_id();
        let packet = RconPacket::new(request_id, SERVERDATA_EXECCOMMAND, command);
        self.send(&packet).await?;
        let sentinel = response_sentinel_packet(sentinel_id);
        self.send(&sentinel).await?;

        let mut packets = Vec::new();
        loop {
            let response = self.receive().await?;
            let is_sentinel = response.request_id == sentinel_id;
            packets.push(response);

            if is_sentinel {
                break;
            }
        }

        reassemble_response_packets(packets, request_id, sentinel_id)
    }

    /// Close the connection
    pub async fn close(&mut self) -> Result<()> {
        if let Some(mut stream) = self.stream.take() {
            stream.shutdown().await?;
        }
        Ok(())
    }

    fn next_id(&self) -> i32 {
        self.request_id.fetch_add(1, Ordering::SeqCst) + 1
    }

    async fn send(&mut self, packet: &RconPacket) -> Result<()> {
        let stream = self.stream.as_mut().context("Not connected")?;
        let data = packet.encode();
        timeout(DEFAULT_TIMEOUT, stream.write_all(&data))
            .await
            .context("Send timed out")?
            .context("Failed to send")?;
        Ok(())
    }

    async fn receive(&mut self) -> Result<RconPacket> {
        let stream = self.stream.as_mut().context("Not connected")?;

        // Read size (4 bytes, little-endian)
        let mut size_buf = [0u8; 4];
        timeout(DEFAULT_TIMEOUT, stream.read_exact(&mut size_buf))
            .await
            .context("Receive timed out")?
            .context("Failed to read size")?;

        let size = i32::from_le_bytes(size_buf) as usize;
        // Factorio can return large responses (especially for entity queries)
        // The Source RCON protocol max is 4096, but Factorio extends this
        if size > 1_000_000 {
            bail!("Packet too large: {} bytes", size);
        }

        // Read rest of packet
        let mut data = vec![0u8; size];
        timeout(DEFAULT_TIMEOUT, stream.read_exact(&mut data))
            .await
            .context("Receive timed out")?
            .context("Failed to read packet")?;

        RconPacket::decode(&data)
    }
}

fn auth_response_complete(packet: &RconPacket, auth_id: i32) -> Result<bool> {
    if packet.request_id == -1 {
        bail!("Authentication failed");
    }

    if packet.request_id != auth_id {
        bail!(
            "Unexpected RCON auth response id: got {}, expected {}",
            packet.request_id,
            auth_id
        );
    }

    match packet.packet_type {
        SERVERDATA_AUTH_RESPONSE => Ok(true),
        SERVERDATA_RESPONSE_VALUE if packet.body.is_empty() => Ok(false),
        SERVERDATA_RESPONSE_VALUE => bail!("Unexpected non-empty RCON auth prelude response"),
        _ => bail!(
            "Unexpected RCON auth response type: got {}, expected {}",
            packet.packet_type,
            SERVERDATA_AUTH_RESPONSE
        ),
    }
}

fn response_sentinel_packet(sentinel_id: i32) -> RconPacket {
    RconPacket::new(sentinel_id, SERVERDATA_EXECCOMMAND, "")
}

fn reassemble_response_packets(
    packets: impl IntoIterator<Item = RconPacket>,
    request_id: i32,
    sentinel_id: i32,
) -> Result<String> {
    let mut body = String::new();
    let mut saw_sentinel = false;

    for packet in packets {
        if packet.request_id == request_id {
            if packet.packet_type != SERVERDATA_RESPONSE_VALUE {
                bail!(
                    "Unexpected RCON response type for request {}: got {}, expected {}",
                    request_id,
                    packet.packet_type,
                    SERVERDATA_RESPONSE_VALUE
                );
            }
            body.push_str(&packet.body);
        } else if packet.request_id == sentinel_id {
            if packet.packet_type != SERVERDATA_RESPONSE_VALUE {
                bail!(
                    "Unexpected RCON sentinel response type for request {}: got {}, expected {}",
                    sentinel_id,
                    packet.packet_type,
                    SERVERDATA_RESPONSE_VALUE
                );
            }
            if !packet.body.is_empty() {
                bail!(
                    "Unexpected non-empty RCON sentinel response for request {}",
                    sentinel_id
                );
            }
            saw_sentinel = true;
            break;
        } else {
            bail!(
                "Unexpected RCON response id: got {}, expected {} or sentinel {}",
                packet.request_id,
                request_id,
                sentinel_id
            );
        }
    }

    if !saw_sentinel {
        bail!(
            "Missing RCON sentinel response for request {} after command request {}",
            sentinel_id,
            request_id
        );
    }

    Ok(body)
}

impl Drop for RconClient {
    fn drop(&mut self) {
        // Can't do async drop, just drop the stream
        self.stream.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_encode_decode() {
        let original = RconPacket::new(1, SERVERDATA_EXECCOMMAND, "test command");
        let encoded = original.encode();

        // Skip the size prefix (first 4 bytes) for decoding
        let decoded = RconPacket::decode(&encoded[4..]).unwrap();

        assert_eq!(original.request_id, decoded.request_id);
        assert_eq!(original.packet_type, decoded.packet_type);
        assert_eq!(original.body, decoded.body);
    }

    #[test]
    fn test_packet_encode_format() {
        let packet = RconPacket::new(1, 2, "hi");
        let encoded = packet.encode();

        // Size = 4 (request_id) + 4 (type) + 2 (body) + 1 (null) + 1 (null) = 12
        assert_eq!(encoded[0..4], 12_i32.to_le_bytes());
        // Request ID = 1
        assert_eq!(encoded[4..8], 1_i32.to_le_bytes());
        // Type = 2
        assert_eq!(encoded[8..12], 2_i32.to_le_bytes());
        // Body = "hi"
        assert_eq!(&encoded[12..14], b"hi");
        // Null terminators
        assert_eq!(encoded[14], 0);
        assert_eq!(encoded[15], 0);
    }

    #[test]
    fn test_auth_response_accepts_empty_prelude_then_auth_response() {
        let prelude = RconPacket::new(42, SERVERDATA_RESPONSE_VALUE, "");
        let auth = RconPacket::new(42, SERVERDATA_AUTH_RESPONSE, "");

        assert!(!auth_response_complete(&prelude, 42).unwrap());
        assert!(auth_response_complete(&auth, 42).unwrap());
    }

    #[test]
    fn test_response_sentinel_is_sent_as_exec_command() {
        let sentinel = response_sentinel_packet(7);

        assert_eq!(sentinel.request_id, 7);
        assert_eq!(sentinel.packet_type, SERVERDATA_EXECCOMMAND);
        assert_eq!(sentinel.body, "");
    }

    #[test]
    fn test_reassembles_fragmented_response_packets() {
        let packet_buffers = [
            RconPacket::new(42, SERVERDATA_RESPONSE_VALUE, "{\"entities\":[").encode(),
            RconPacket::new(42, SERVERDATA_RESPONSE_VALUE, "{\"name\":\"iron-ore\"}").encode(),
            RconPacket::new(7, SERVERDATA_RESPONSE_VALUE, "").encode(),
        ];
        let packets = packet_buffers
            .iter()
            .map(|packet| RconPacket::decode(&packet[4..]).unwrap());

        let body = reassemble_response_packets(packets, 42, 7).unwrap();

        assert_eq!(body, "{\"entities\":[{\"name\":\"iron-ore\"}");
    }
}
