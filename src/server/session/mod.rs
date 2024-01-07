use sdp::description::session::Origin;
use sdp::SessionDescription;
use shared::error::{Error, Result};
use std::cell::RefCell;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::rc::Rc;

pub mod description;

use crate::server::certificate::RTCCertificate;
use crate::server::endpoint::{
    candidate::{Candidate, ConnectionCredentials},
    Endpoint,
};
use crate::server::session::description::rtp_codec::RTPCodecType;
use crate::server::session::description::rtp_transceiver::RTCRtpTransceiver;
use crate::server::session::description::rtp_transceiver_direction::RTCRtpTransceiverDirection;
use crate::server::session::description::sdp_type::RTCSdpType;
use crate::server::session::description::{
    find_by_mid, get_mid_value, get_peer_direction, get_rids, populate_sdp, update_sdp_origin,
    MediaSection, RTCSessionDescription, MEDIA_SECTION_APPLICATION,
};
use crate::shared::types::{EndpointId, SessionId, UserName};

#[derive(Debug)]
pub struct Session {
    session_id: SessionId,
    local_addr: SocketAddr,
    certificate: RTCCertificate,
    endpoints: RefCell<HashMap<EndpointId, Rc<Endpoint>>>,
    candidates: RefCell<HashMap<UserName, Rc<Candidate>>>,
}

impl Session {
    pub fn new(session_id: SessionId, local_addr: SocketAddr, certificate: RTCCertificate) -> Self {
        Self {
            session_id,
            local_addr,
            certificate,
            endpoints: RefCell::new(HashMap::new()),
            candidates: RefCell::new(HashMap::new()),
        }
    }

    pub fn session_id(&self) -> u64 {
        self.session_id
    }

    pub(crate) fn add_candidate(&self, candidate: Rc<Candidate>) -> bool {
        let username = candidate.username();
        let mut candidates = self.candidates.borrow_mut();
        candidates.insert(username, candidate).is_some()
    }

    //TODO: add idle timeout to remove candidate
    pub(crate) fn remove_candidate(&self, candidate: &Rc<Candidate>) -> bool {
        let username = candidate.username();
        let mut candidates = self.candidates.borrow_mut();
        candidates.remove(&username).is_some()
    }

    pub(crate) fn find_candidate(&self, username: &UserName) -> Option<Rc<Candidate>> {
        let candidates = self.candidates.borrow();
        candidates.get(username).cloned()
    }

    // set pending offer and return answer
    pub fn accept_pending_offer(
        &self,
        endpoint_id: EndpointId,
        mut offer: RTCSessionDescription,
    ) -> Result<RTCSessionDescription> {
        offer.unmarshal()?;
        let parsed = offer.unmarshal()?;
        let remote_conn_cred = ConnectionCredentials::from_sdp(&parsed)?;
        offer.parsed = Some(parsed);

        let mut candidate = Candidate::new(
            self.session_id,
            endpoint_id,
            &self.certificate,
            remote_conn_cred,
            offer,
        );

        let answer = self.create_pending_answer(
            candidate.remote_description().parsed.as_ref().unwrap(),
            candidate.local_connection_credentials(),
        )?;
        candidate.set_local_description(&answer);

        self.add_candidate(Rc::new(candidate));

        Ok(answer)
    }

    pub fn create_pending_answer(
        &self,
        remote_description: &SessionDescription,
        local_connection_credentials: &ConnectionCredentials,
    ) -> Result<RTCSessionDescription> {
        let use_identity = false; //TODO: self.config.idp_login_url.is_some();
        let local_transceivers = vec![]; //TODO: self.get_transceivers();
        let mut d = self.generate_matched_sdp(
            remote_description,
            local_connection_credentials,
            local_transceivers,
            use_identity,
            false, /*includeUnmatched */
        )?;

        let mut sdp_origin = Origin::default();
        update_sdp_origin(&mut sdp_origin, &mut d);

        let sdp = d.marshal();

        let answer = RTCSessionDescription {
            sdp_type: RTCSdpType::Answer,
            sdp,
            parsed: Some(d),
        };

        Ok(answer)
    }

    /// generate_matched_sdp generates a SDP and takes the remote state into account
    /// this is used everytime we have a remote_description
    pub(crate) fn generate_matched_sdp(
        &self,
        remote_description: &SessionDescription,
        local_connection_credentials: &ConnectionCredentials,
        mut local_transceivers: Vec<RTCRtpTransceiver>,
        use_identity: bool,
        include_unmatched: bool,
    ) -> Result<SessionDescription> {
        let d = SessionDescription::new_jsep_session_description(use_identity);

        let mut media_sections = vec![];
        let mut already_have_application_media_section = false;
        for media in &remote_description.media_descriptions {
            if let Some(mid_value) = get_mid_value(media) {
                if mid_value.is_empty() {
                    return Err(Error::Other(
                        "ErrPeerConnRemoteDescriptionWithoutMidValue".to_string(),
                    ));
                }

                if media.media_name.media == MEDIA_SECTION_APPLICATION {
                    media_sections.push(MediaSection {
                        id: mid_value.to_owned(),
                        data: true,
                        ..Default::default()
                    });
                    already_have_application_media_section = true;
                    continue;
                }

                let kind = RTPCodecType::from(media.media_name.media.as_str());
                let direction = get_peer_direction(media);
                if kind == RTPCodecType::Unspecified
                    || direction == RTCRtpTransceiverDirection::Unspecified
                {
                    continue;
                }

                if let Some(mut t) = find_by_mid(mid_value, &mut local_transceivers) {
                    t.sender.negotiated = true;
                    let media_transceivers = vec![t];

                    #[allow(clippy::unnecessary_lazy_evaluations)]
                    media_sections.push(MediaSection {
                        id: mid_value.to_owned(),
                        transceivers: media_transceivers,
                        rid_map: get_rids(media),
                        offered_direction: (!include_unmatched).then(|| direction),
                        ..Default::default()
                    });
                } else {
                    return Err(Error::Other("ErrPeerConnTransceiverMidNil".to_string()));
                }
            }
        }

        // If we are offering also include unmatched local transceivers
        if include_unmatched {
            for t in &mut local_transceivers {
                t.sender.negotiated = true;
                media_sections.push(MediaSection {
                    id: t.mid.clone(),
                    transceivers: vec![t.clone()],
                    ..Default::default()
                });
            }

            if !already_have_application_media_section {
                media_sections.push(MediaSection {
                    id: format!("{}", media_sections.len()),
                    data: true,
                    ..Default::default()
                });
            }
        }

        populate_sdp(
            d,
            self.certificate.get_fingerprints(),
            &self.local_addr,
            local_connection_credentials,
            &media_sections,
            true,
        )
    }
}
