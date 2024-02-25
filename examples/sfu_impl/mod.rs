#![allow(dead_code)]

use bytes::Bytes;
use rouille::{Request, Response, ResponseBody};
use sfu::{RTCSessionDescription, ServerStates};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Error, ErrorKind, Read};
use std::net::UdpSocket;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{mpsc, Arc};

// Handle a web request.
pub fn web_request_sfu(
    request: &Request,
    _host: &str,
    media_port_thread_map: Arc<
        HashMap<
            u16,
            (
                Option<SyncSender<str0m::Rtc>>,
                Option<SyncSender<SignalingMessage>>,
            ),
        >,
    >,
) -> Response {
    if request.method() == "GET" {
        return Response::html(include_str!("../chat.html"));
    }

    // "/offer/433774451/456773342" or "/leave/433774451/456773342"
    let path: Vec<String> = request.url().split('/').map(|s| s.to_owned()).collect();
    if path.len() != 4 || path[2].parse::<u64>().is_err() || path[3].parse::<u64>().is_err() {
        return Response::empty_400();
    }

    let session_id = path[2].parse::<u64>().unwrap();
    let mut sorted_ports: Vec<u16> = media_port_thread_map.keys().map(|x| *x).collect();
    sorted_ports.sort();
    assert!(!sorted_ports.is_empty());
    let port = sorted_ports[(session_id as usize) % sorted_ports.len()];
    let (_, tx) = media_port_thread_map.get(&port).unwrap();

    // Expected POST SDP Offers.
    let mut offer_sdp = vec![];
    request
        .data()
        .expect("body to be available")
        .read_to_end(&mut offer_sdp)
        .unwrap();

    // The Rtc instance is shipped off to the main run loop.
    if let Some(tx) = tx {
        let endpoint_id = path[3].parse::<u64>().unwrap();
        if path[1] == "offer" {
            let (response_tx, response_rx) = mpsc::sync_channel(1);

            tx.send(SignalingMessage {
                request: SignalingProtocolMessage::Offer {
                    session_id,
                    endpoint_id,
                    offer_sdp: Bytes::from(offer_sdp),
                },
                response_tx,
            })
            .expect("to send SignalingMessage instance");

            let response = response_rx.recv().expect("receive answer offer");
            match response {
                SignalingProtocolMessage::Answer {
                    session_id: _,
                    endpoint_id: _,
                    answer_sdp,
                } => Response::from_data("application/json", answer_sdp),
                _ => Response::empty_404(),
            }
        } else {
            // leave
            Response {
                status_code: 200,
                headers: vec![],
                data: ResponseBody::empty(),
                upgrade: None,
            }
        }
    } else {
        Response::empty_406()
    }
}

/// This is the "main run loop" that handles all clients, reads and writes UdpSocket traffic,
/// and forwards media data between clients.
pub fn run_sfu(_socket: UdpSocket, _rx: Receiver<SignalingMessage>) -> anyhow::Result<()> {
    //let mut clients: Vec<Client> = vec![];
    //let mut to_propagate: VecDeque<Propagated> = VecDeque::new();
    //let mut buf = vec![0; 2000];

    /*loop {
        // Clean out disconnected clients
        clients.retain(|c| c.rtc.is_alive());

        // Spawn new incoming clients from the web server thread.
        if let Some(mut client) = spawn_new_client(&rx) {
            // Add incoming tracks present in other already connected clients.
            for track in clients.iter().flat_map(|c| c.tracks_in.iter()) {
                let weak = Arc::downgrade(&track.id);
                client.handle_track_open(weak);
            }

            clients.push(client);
        }

        // Poll clients until they return timeout
        let mut timeout = Instant::now() + Duration::from_millis(100);
        for client in clients.iter_mut() {
            let t = poll_until_timeout(client, &mut to_propagate, &socket);
            timeout = timeout.min(t);
        }

        // If we have an item to propagate, do that
        if let Some(p) = to_propagate.pop_front() {
            propagate(&p, &mut clients);
            continue;
        }

        // The read timeout is not allowed to be 0. In case it is 0, we set 1 millisecond.
        let duration = (timeout - Instant::now()).max(Duration::from_millis(1));

        socket
            .set_read_timeout(Some(duration))
            .expect("setting socket read timeout");

        if let Some(input) = read_socket_input(&socket, &mut buf) {
            // The rtc.accepts() call is how we demultiplex the incoming packet to know which
            // Rtc instance the traffic belongs to.
            if let Some(client) = clients.iter_mut().find(|c| c.accepts(&input)) {
                // We found the client that accepts the input.
                client.handle_input(input);
            } else {
                // This is quite common because we don't get the Rtc instance via the mpsc channel
                // quickly enough before the browser send the first STUN.
                debug!("No client accepts UDP input: {:?}", input);
            }
        }

        // Drive time forward in all clients.
        let now = Instant::now();
        for client in &mut clients {
            client.handle_input(Input::Timeout(now));
        }
    }*/
    Ok(())
}

pub enum SignalingProtocolMessage {
    Ok {
        session_id: u64,
        endpoint_id: u64,
    },
    Err {
        session_id: u64,
        endpoint_id: u64,
        reason: Bytes,
    },
    Offer {
        session_id: u64,
        endpoint_id: u64,
        offer_sdp: Bytes,
    },
    Answer {
        session_id: u64,
        endpoint_id: u64,
        answer_sdp: Bytes,
    },
    Leave {
        session_id: u64,
        endpoint_id: u64,
    },
}

pub struct SignalingMessage {
    pub request: SignalingProtocolMessage,
    pub response_tx: SyncSender<SignalingProtocolMessage>,
}

pub fn handle_signaling_message(
    server_states: &Rc<RefCell<ServerStates>>,
    signaling_msg: SignalingMessage,
) -> anyhow::Result<()> {
    match signaling_msg.request {
        SignalingProtocolMessage::Offer {
            session_id,
            endpoint_id,
            offer_sdp,
        } => handle_offer_message(
            server_states,
            session_id,
            endpoint_id,
            offer_sdp,
            signaling_msg.response_tx,
        ),
        SignalingProtocolMessage::Leave {
            session_id,
            endpoint_id,
        } => handle_leave_message(
            server_states,
            session_id,
            endpoint_id,
            signaling_msg.response_tx,
        ),
        SignalingProtocolMessage::Ok {
            session_id,
            endpoint_id,
        }
        | SignalingProtocolMessage::Err {
            session_id,
            endpoint_id,
            reason: _,
        }
        | SignalingProtocolMessage::Answer {
            session_id,
            endpoint_id,
            answer_sdp: _,
        } => Ok(signaling_msg
            .response_tx
            .send(SignalingProtocolMessage::Err {
                session_id,
                endpoint_id,
                reason: Bytes::from("Invalid Request"),
            })
            .map_err(|_| {
                Error::new(
                    ErrorKind::Other,
                    "failed to send back signaling message response".to_string(),
                )
            })?),
    }
}

fn handle_offer_message(
    server_states: &Rc<RefCell<ServerStates>>,
    session_id: u64,
    endpoint_id: u64,
    offer: Bytes,
    response_tx: SyncSender<SignalingProtocolMessage>,
) -> anyhow::Result<()> {
    let try_handle = || -> anyhow::Result<Bytes> {
        let offer_str = String::from_utf8(offer.to_vec())?;
        log::info!(
            "handle_offer_message: {}/{}/{}",
            session_id,
            endpoint_id,
            offer_str,
        );
        let mut server_states = server_states.borrow_mut();

        let offer_sdp = serde_json::from_str::<RTCSessionDescription>(&offer_str)?;
        let answer = server_states.accept_offer(session_id, endpoint_id, None, offer_sdp)?;
        let answer_str = serde_json::to_string(&answer)?;
        log::info!("generate answer sdp: {}", answer_str);
        Ok(Bytes::from(answer_str))
    };

    match try_handle() {
        Ok(answer_sdp) => Ok(response_tx
            .send(SignalingProtocolMessage::Answer {
                session_id,
                endpoint_id,
                answer_sdp,
            })
            .map_err(|_| {
                Error::new(
                    ErrorKind::Other,
                    "failed to send back signaling message response".to_string(),
                )
            })?),
        Err(err) => Ok(response_tx
            .send(SignalingProtocolMessage::Err {
                session_id,
                endpoint_id,
                reason: Bytes::from(err.to_string()),
            })
            .map_err(|_| {
                Error::new(
                    ErrorKind::Other,
                    "failed to send back signaling message response".to_string(),
                )
            })?),
    }
}

fn handle_leave_message(
    _server_states: &Rc<RefCell<ServerStates>>,
    session_id: u64,
    endpoint_id: u64,
    response_tx: SyncSender<SignalingProtocolMessage>,
) -> anyhow::Result<()> {
    let try_handle = || -> anyhow::Result<()> {
        log::info!("handle_leave_message: {}/{}", session_id, endpoint_id,);
        Ok(())
    };

    match try_handle() {
        Ok(_) => Ok(response_tx
            .send(SignalingProtocolMessage::Ok {
                session_id,
                endpoint_id,
            })
            .map_err(|_| {
                Error::new(
                    ErrorKind::Other,
                    "failed to send back signaling message response".to_string(),
                )
            })?),
        Err(err) => Ok(response_tx
            .send(SignalingProtocolMessage::Err {
                session_id,
                endpoint_id,
                reason: Bytes::from(err.to_string()),
            })
            .map_err(|_| {
                Error::new(
                    ErrorKind::Other,
                    "failed to send back signaling message response".to_string(),
                )
            })?),
    }
}
