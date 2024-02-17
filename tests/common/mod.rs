#![allow(dead_code)]

use anyhow::Result;
use hyper::{Body, Client, Method, Request};
use log::LevelFilter::Debug;
use log::{error, info};
use rand::random;
use sfu::{EndpointId, SessionId};
use std::io::Write;
use std::sync::Arc;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::sdp_type::RTCSdpType;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;

pub async fn setup(config: RTCConfiguration) -> Result<(Arc<RTCPeerConnection>, EndpointId)> {
    env_logger::Builder::new()
        .format(|buf, record| {
            writeln!(
                buf,
                "{}:{} [{}] {} - {}",
                record.file().unwrap_or("unknown"),
                record.line().unwrap_or(0),
                record.level(),
                chrono::Local::now().format("%H:%M:%S.%6f"),
                record.args()
            )
        })
        .filter(None, Debug)
        .try_init()?;

    // some setup code, like creating required files/directories, starting
    // servers, etc.
    info!("common setup");

    // Create a MediaEngine object to configure the supported codec
    let mut m = MediaEngine::default();

    // Register default codecs
    m.register_default_codecs()?;

    // Create a InterceptorRegistry. This is the user configurable RTP/RTCP Pipeline.
    // This provides NACKs, RTCP Reports and other features. If you use `webrtc.NewPeerConnection`
    // this is enabled by default. If you are manually managing You MUST create a InterceptorRegistry
    // for each PeerConnection.
    let mut registry = Registry::new();

    // Use the default set of Interceptors
    registry = register_default_interceptors(registry, &mut m)?;

    // Create the API object with the MediaEngine
    let api = APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .build();

    // Create a new RTCPeerConnection
    let peer_connection = Arc::new(api.new_peer_connection(config).await?);

    // Set the handler for Peer connection state
    // This will notify you when the peer has connected/disconnected
    peer_connection.on_peer_connection_state_change(Box::new(move |s: RTCPeerConnectionState| {
        info!("Peer Connection State has changed: {s}");

        if s == RTCPeerConnectionState::Failed {
            // Wait until PeerConnection has had no network activity for 30 seconds or another failure. It may be reconnected using an ICE Restart.
            // Use webrtc.PeerConnectionStateDisconnected if you are interested in detecting faster timeout.
            // Note that the PeerConnection may come back from PeerConnectionStateDisconnected.
            error!("Peer Connection has gone to failed exiting");
            assert!(false);
        }

        Box::pin(async {})
    }));

    Ok((peer_connection, random::<u64>()))
}

pub async fn teardown(pc: Arc<RTCPeerConnection>) -> Result<()> {
    pc.close().await?;

    Ok(())
}

async fn signaling(
    host: String,
    signal_port: u16,
    session_id: SessionId,
    endpoint_id: EndpointId,
    offer_payload: String,
) -> Result<RTCSessionDescription> {
    info!("connecting to signaling server http://{host}:{signal_port}/offer/{session_id}/{endpoint_id}");
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!(
            "http://{host}:{signal_port}/offer/{session_id}/{endpoint_id}"
        ))
        .header("content-type", "application/json; charset=utf-8")
        .body(Body::from(offer_payload))?;

    let resp = Client::new().request(req).await?;
    let answer_payload =
        std::str::from_utf8(&hyper::body::to_bytes(resp.into_body()).await?)?.to_string();
    let answer = serde_json::from_str::<RTCSessionDescription>(&answer_payload)?;

    Ok(answer)
}

pub async fn connect(
    host: String,
    signal_port: u16,
    session_id: SessionId,
    endpoint_id: EndpointId,
    peer_connection: &Arc<RTCPeerConnection>,
) -> Result<()> {
    // Create a datachannel with label 'data'
    let data_channel = peer_connection.create_data_channel("data", None).await?;

    // Register SDP message handling
    let peer_connection_clone = peer_connection.clone();
    let data_channel_clone = data_channel.clone();
    data_channel.on_message(Box::new(move |msg: DataChannelMessage| {
        let sdp_str = String::from_utf8(msg.data.to_vec()).unwrap();
        info!("SDP from DataChannel: '{sdp_str}'");
        let sdp = match serde_json::from_str::<RTCSessionDescription>(&sdp_str) {
            Ok(sdp) => sdp,
            Err(err) => {
                error!("deserialize sdp str failed: {}", err);
                assert!(false);
                return Box::pin(async {});
            }
        };
        let pc = peer_connection_clone.clone();
        let dc = data_channel_clone.clone();
        Box::pin(async move {
            match sdp.sdp_type {
                RTCSdpType::Offer => {
                    if let Err(err) = pc.set_remote_description(sdp).await {
                        error!("set_remote_description offer error {:?}", err);
                        assert!(false);
                        return;
                    }

                    // Create an answer to send to the other process
                    let answer = match pc.create_answer(None).await {
                        Ok(a) => a,
                        Err(err) => {
                            error!("create_answer error {:?}", err);
                            assert!(false);
                            return;
                        }
                    };

                    let answer_str = match serde_json::to_string(&answer) {
                        Ok(a) => a,
                        Err(err) => {
                            error!("serialize answer error {:?}", err);
                            assert!(false);
                            return;
                        }
                    };
                    if let Err(err) = dc.send_text(answer_str).await {
                        error!("data channel send answer error {:?}", err);
                        assert!(false);
                        return;
                    }
                }
                RTCSdpType::Answer => {
                    if let Err(err) = pc.set_remote_description(sdp).await {
                        error!("set_remote_description answer error {:?}", err);
                        assert!(false);
                        return;
                    }
                }
                _ => {
                    error!("Unsupported SDP type {}", sdp.sdp_type);
                    assert!(false);
                }
            }
        })
    }));

    // Create an offer to send to the other process
    let offer = peer_connection.create_offer(None).await?;

    // Send our offer to the HTTP server listening in the other process
    let offer_payload = serde_json::to_string(&offer)?;

    // Sets the LocalDescription, and starts our UDP listeners
    // Note: this will start the gathering of ICE candidates
    peer_connection.set_local_description(offer).await?;

    let answer = signaling(host, signal_port, session_id, endpoint_id, offer_payload).await?;
    peer_connection.set_remote_description(answer).await?;

    Ok(())
}
