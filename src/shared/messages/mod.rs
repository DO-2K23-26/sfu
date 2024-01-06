use bytes::BytesMut;
use retty::transport::TransportContext;
use std::time::Instant;

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub(crate) enum DataChannelMessageType {
    None,
    Control,
    Binary,
    Text,
}

#[derive(Debug)]
pub(crate) enum DataChannelMessageParams {
    Inbound {
        seq_num: u16,
    },
    Outbound {
        ordered: bool,
        reliable: bool,
        max_rtx_count: u32,
        max_rtx_millis: u32,
    },
}

#[derive(Debug)]
pub struct DataChannelMessage {
    pub(crate) association_handle: usize,
    pub(crate) stream_id: u16,
    pub(crate) data_message_type: DataChannelMessageType,
    pub(crate) params: DataChannelMessageParams,
    pub(crate) payload: BytesMut,
}

#[derive(Debug)]
pub enum DTLSMessageEvent {
    RAW(BytesMut),
    SCTP(DataChannelMessage),
    APPLICATION(BytesMut),
}

#[derive(Debug)]
pub enum RTPMessageEvent {
    RAW(BytesMut),
    RTP(rtp::packet::Packet),
    RTCP(Vec<Box<dyn rtcp::packet::Packet + Send + Sync>>),
}

#[derive(Debug)]
pub enum MessageEvent {
    DTLS(DTLSMessageEvent),
    RTP(RTPMessageEvent),
}

pub struct TaggedMessageEvent {
    pub now: Instant,
    pub transport: TransportContext,
    pub message: MessageEvent,
}
