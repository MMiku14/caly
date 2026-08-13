//! Unambiguous canonical node-identity hashing.

use sha2::{Digest, Sha256};

use crate::{NodeId, SecretText};

use super::{DialableNode, Transport};

pub(super) fn node_id(node: &DialableNode) -> NodeId {
    let mut encoder = CanonicalEncoder::new();
    encoder.text("caly-node-identity-v1");
    encoder.text(node.endpoint().host().as_str());
    encoder.u16(node.endpoint().port().get());
    protocol_encode::encode(&mut encoder, node.protocol());
    encode_transport(&mut encoder, node.transport());
    encode_tls(&mut encoder, node.tls());
    encoder.optional(node.dialer_proxy(), |writer, id| {
        writer.bytes(&id.into_bytes());
    });
    encoder.finish()
}

mod protocol_encode;

struct CanonicalEncoder(Sha256);

impl CanonicalEncoder {
    fn new() -> Self {
        Self(Sha256::new())
    }

    fn bytes(&mut self, value: &[u8]) {
        self.0.update((value.len() as u64).to_be_bytes());
        self.0.update(value);
    }

    fn text(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }
    fn u8(&mut self, value: u8) {
        self.bytes(&[value]);
    }
    fn u16(&mut self, value: u16) {
        self.bytes(&value.to_be_bytes());
    }
    fn u32(&mut self, value: u32) {
        self.bytes(&value.to_be_bytes());
    }
    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn optional<T>(&mut self, value: Option<&T>, encode: impl FnOnce(&mut Self, &T)) {
        match value {
            Some(value) => {
                self.u8(1);
                encode(self, value);
            }
            None => self.u8(0),
        }
    }

    fn secret<const MAX: usize>(&mut self, value: &SecretText<MAX>) {
        value.with_exposed(|secret| self.text(secret));
    }

    fn finish(self) -> NodeId {
        let digest = self.0.finalize();
        let mut identity = [0_u8; 16];
        identity.copy_from_slice(&digest[..16]);
        NodeId::from_bytes(identity)
    }
}

fn encode_transport(writer: &mut CanonicalEncoder, transport: Option<&Transport>) {
    writer.optional(transport, |out, value| match value {
        Transport::Tcp => out.text("tcp"),
        Transport::WebSocket {
            path,
            host,
            early_data,
        } => {
            out.text("ws");
            out.text(path.as_str());
            optional_text(out, host.as_ref());
            match early_data {
                Some(early) => {
                    out.text(early.header_name().as_str());
                    out.u32(early.max_bytes());
                }
                None => out.u32(0),
            }
        }
        Transport::Grpc { service_name } => {
            out.text("grpc");
            out.text(service_name.as_str());
        }
        Transport::Http2 { path, hosts } => {
            out.text("h2");
            out.text(path.as_str());
            out.u32(bounded_count(hosts.len()));
            for host in hosts {
                out.text(host.as_str());
            }
        }
        Transport::Quic => out.text("quic"),
    });
}

fn encode_tls(writer: &mut CanonicalEncoder, tls: Option<&super::TlsConfig>) {
    writer.optional(tls, |out, value| {
        optional_text(out, value.sni());
        out.u32(bounded_count(value.alpn().len()));
        for alpn in value.alpn() {
            out.text(alpn.as_str());
        }
        out.bool(value.allow_insecure());
        optional_text(out, value.fingerprint());
        out.optional(value.reality(), |inner, reality| {
            inner.secret(reality.public_key());
            inner.optional(reality.short_id(), CanonicalEncoder::secret);
            optional_text(inner, reality.spider_x());
        });
    });
}

fn optional_text(writer: &mut CanonicalEncoder, value: Option<&super::ProtocolText>) {
    writer.optional(value, |out, text| out.text(text.as_str()));
}

#[allow(clippy::cast_possible_truncation)]
fn bounded_count(value: usize) -> u32 {
    debug_assert!(u32::try_from(value).is_ok());
    value as u32
}
