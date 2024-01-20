use crate::server::config::{ServerConfig, SessionConfig};
use crate::server::endpoint::candidate::{Candidate, ConnectionCredentials};
use crate::server::session::description::RTCSessionDescription;
use crate::server::session::Session;
use crate::types::{EndpointId, SessionId, UserName};
use shared::error::{Error, Result};
use srtp::context::Context;
use std::cell::RefCell;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

pub struct ServerStates {
    server_config: Arc<ServerConfig>,
    local_addr: SocketAddr,
    sessions: RefCell<HashMap<SessionId, Rc<Session>>>,

    //TODO: add idle timeout cleanup logic to remove idle endpoint and candidates
    candidates: RefCell<HashMap<UserName, Rc<Candidate>>>,
    local_srtp_contexts: RefCell<HashMap<SocketAddr, Context>>,
    remote_srtp_contexts: RefCell<HashMap<SocketAddr, Context>>,
    //endpoints: RefCell<HashMap<FourTuple, Rc<Endpoint>>>,
}

impl ServerStates {
    /// create new server states
    pub fn new(server_config: Arc<ServerConfig>, local_addr: SocketAddr) -> Result<Self> {
        let _ = server_config
            .certificates
            .first()
            .ok_or(Error::ErrInvalidCertificate)?
            .get_fingerprints()
            .first()
            .ok_or(Error::ErrInvalidCertificate)?;

        Ok(Self {
            server_config,
            local_addr,
            sessions: RefCell::new(HashMap::new()),

            candidates: RefCell::new(HashMap::new()),
            local_srtp_contexts: RefCell::new(HashMap::new()),
            remote_srtp_contexts: RefCell::new(HashMap::new()),
            //endpoints: RefCell::new(HashMap::new()),
        })
    }

    /// accept offer and return answer
    pub fn accept_offer(
        &self,
        session_id: SessionId,
        endpoint_id: EndpointId,
        mut offer: RTCSessionDescription,
    ) -> Result<RTCSessionDescription> {
        offer.unmarshal()?;
        let parsed = offer.unmarshal()?;
        let remote_conn_cred = ConnectionCredentials::from_sdp(&parsed)?;
        offer.parsed = Some(parsed);

        let local_conn_cred = ConnectionCredentials::new(
            &self.server_config.certificates,
            remote_conn_cred.dtls_params.role,
        );

        let session = self.create_or_get_session(session_id);
        let answer =
            session.create_pending_answer(endpoint_id, &offer, &local_conn_cred.ice_params)?;

        self.add_candidate(Rc::new(Candidate::new(
            session.session_id(),
            endpoint_id,
            remote_conn_cred,
            local_conn_cred,
            offer,
            answer.clone(),
            Instant::now() + self.server_config.candidate_idle_timeout,
        )));

        Ok(answer)
    }

    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub(crate) fn local_srtp_contexts(&self) -> &RefCell<HashMap<SocketAddr, Context>> {
        &self.local_srtp_contexts
    }

    pub(crate) fn remote_srtp_contexts(&self) -> &RefCell<HashMap<SocketAddr, Context>> {
        &self.remote_srtp_contexts
    }

    pub(crate) fn create_or_get_session(&self, session_id: SessionId) -> Rc<Session> {
        let mut sessions = self.sessions.borrow_mut();
        if let Some(session) = sessions.get(&session_id) {
            session.clone()
        } else {
            let session = Rc::new(Session::new(
                SessionConfig::new(Arc::clone(&self.server_config), self.local_addr),
                session_id,
            ));
            sessions.insert(session_id, Rc::clone(&session));
            session
        }
    }

    pub(crate) fn get_session(&self, session_id: &SessionId) -> Option<Rc<Session>> {
        self.sessions.borrow().get(session_id).cloned()
    }

    pub(crate) fn add_candidate(&self, candidate: Rc<Candidate>) -> Option<Rc<Candidate>> {
        let username = candidate.username();
        let mut candidates = self.candidates.borrow_mut();
        candidates.insert(username, candidate)
    }

    pub(crate) fn remove_candidate(&self, username: &UserName) -> Option<Rc<Candidate>> {
        let mut candidates = self.candidates.borrow_mut();
        candidates.remove(username)
    }

    pub(crate) fn find_candidate(&self, username: &UserName) -> Option<Rc<Candidate>> {
        let candidates = self.candidates.borrow();
        candidates.get(username).cloned()
    }
}
