//! In-memory MOD lifecycle only; Ready is not a grant. No execution or transport wiring.
use super::{ensure, Check, Error, Message, Reply, ValidatedManifest, API_VERSION};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

pub const MAX_REQUESTS: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    AwaitingHandshake,
    Ready,
    Closing,
    Closed,
    Failed,
}

#[derive(Debug)]
pub enum Outcome {
    Reply(Reply),
    /// Local result delivery was suppressed; remote side effects may still have occurred.
    /// This is not an acknowledgement that remote execution was cancelled.
    LocalCancelled,
    TimedOut,
    Closed,
    Failed,
}

#[derive(Debug)]
pub struct Completion {
    pub id: String,
    pub outcome: Outcome,
}

struct Pending {
    request: Message,
    deadline: Instant,
    cancelling: bool,
}

pub struct Session {
    manifest: ValidatedManifest,
    generation: u64,
    state: State,
    issued: u64,
    pending: BTreeMap<String, Pending>,
}

impl Session {
    /// Host must use a fresh generation for every replacement session, without wrapping,
    /// and bind it to the source connection, never to an untrusted wire field.
    pub fn new(manifest: ValidatedManifest, generation: u64) -> Self {
        Self {
            manifest,
            generation,
            state: State::AwaitingHandshake,
            issued: 0,
            pending: BTreeMap::new(),
        }
    }

    pub fn state(&self) -> State {
        self.state
    }
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// Earliest request deadline for host scheduling; None means no pending work,
    /// not that the process has exited. Cancellation keeps the original deadline.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().map(|pending| pending.deadline).min()
    }

    pub fn request(&mut self, tool: &str, params: Value, now: Instant) -> Check<Message> {
        ensure(self.state == State::Ready, "session not ready")?;
        ensure(self.issued < MAX_REQUESTS, "session request lifetime limit")?;
        let limits = &self.manifest.manifest().limits;
        ensure(
            self.pending.len() < limits.max_in_flight as usize,
            "in-flight limit",
        )?;
        let deadline = now
            .checked_add(Duration::from_millis(limits.timeout_ms as u64))
            .ok_or(Error("deadline overflow"))?;
        let id = format!("r{}", self.issued + 1);
        let request = Message::Request {
            api_version: API_VERSION,
            id: id.clone(),
            tool: tool.into(),
            params,
        };
        request.validate(&self.manifest)?;
        self.pending.insert(
            id,
            Pending {
                request: request.clone(),
                deadline,
                cancelling: false,
            },
        );
        self.issued += 1;
        Ok(request)
    }

    /// Only plugin handshakes/results are accepted. Rejections never consume pending work.
    /// Use the same monotonic clock as request/expire; run expire to collect overdue outcomes.
    pub fn incoming(
        &mut self,
        generation: u64,
        message: Message,
        now: Instant,
    ) -> Check<Option<Completion>> {
        ensure(generation == self.generation, "stale session generation")?;
        match &message {
            Message::Handshake { .. } => {
                ensure(
                    self.state == State::AwaitingHandshake,
                    "unexpected handshake",
                )?;
                message.validate(&self.manifest)?;
                self.state = State::Ready;
                Ok(None)
            }
            Message::Result { id, .. } => {
                ensure(self.state == State::Ready, "unexpected result")?;
                let pending = self.pending.get(id).ok_or(Error("unknown result id"))?;
                message.validate_reply_to(&pending.request, &self.manifest)?;
                ensure(
                    now < pending.deadline,
                    "result deadline elapsed; expire required",
                )?;
                let cancelling = pending.cancelling;
                self.pending.remove(id);
                let Message::Result { id, outcome, .. } = message else {
                    unreachable!()
                };
                Ok(Some(Completion {
                    id,
                    outcome: if cancelling {
                        Outcome::LocalCancelled
                    } else {
                        Outcome::Reply(outcome)
                    },
                }))
            }
            _ => Err(Error("unexpected plugin message direction")),
        }
    }

    /// Suppress subsequent result delivery locally, retaining the slot until a valid
    /// terminal reply or the original deadline. Remote side effects may still occur;
    /// returning a Cancel message does not send it or acknowledge remote cancellation.
    /// Repeated cancellation is rejected.
    pub fn cancel(&mut self, id: &str) -> Check<Message> {
        ensure(self.state == State::Ready, "session not ready")?;
        let pending = self.pending.get_mut(id).ok_or(Error("unknown cancel id"))?;
        ensure(!pending.cancelling, "already cancelling")?;
        let message = Message::Cancel {
            api_version: API_VERSION,
            id: id.into(),
        };
        message.validate(&self.manifest)?;
        pending.cancelling = true;
        Ok(message)
    }

    /// At most max_in_flight outcomes; a timeout says nothing about process liveness.
    pub fn expire(&mut self, now: Instant) -> Vec<Completion> {
        let mut completed = Vec::new();
        self.pending.retain(|id, pending| {
            if now < pending.deadline {
                return true;
            }
            completed.push(Completion {
                id: id.clone(),
                outcome: if pending.cancelling {
                    Outcome::LocalCancelled
                } else {
                    Outcome::TimedOut
                },
            });
            false
        });
        completed
    }

    /// Locally abandon pending work exactly once. No wire close or process kill exists here.
    pub fn close(&mut self) -> Vec<Completion> {
        if matches!(self.state, State::Closing | State::Closed | State::Failed) {
            return Vec::new();
        }
        self.state = State::Closing;
        self.drain(false)
    }

    pub fn finish_close(&mut self) -> Check {
        ensure(self.state == State::Closing, "session not closing")?;
        self.state = State::Closed;
        Ok(())
    }

    /// An explicit host-observed failure, not an inference from a request timeout.
    pub fn fail(&mut self) -> Vec<Completion> {
        if matches!(self.state, State::Closed | State::Failed) {
            return Vec::new();
        }
        self.state = State::Failed;
        self.drain(true)
    }

    fn drain(&mut self, failed: bool) -> Vec<Completion> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|(id, pending)| Completion {
                id,
                outcome: if pending.cancelling {
                    Outcome::LocalCancelled
                } else if failed {
                    Outcome::Failed
                } else {
                    Outcome::Closed
                },
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TOOL: &str = "mod_org_example__echo";
    fn manifest(slots: u32) -> ValidatedManifest {
        let schema =
            json!({"type":"object", "properties":{}, "required":[], "additionalProperties":false});
        ValidatedManifest::parse(&serde_json::to_vec(&json!({
            "api_version":1, "id":"org.example", "revision":1, "executable":"bin/mod.exe",
            "capabilities":[], "limits":{"timeout_ms":10, "memory_mib":64,
                "max_in_flight":slots, "max_message_bytes":65536, "max_result_bytes":32768},
            "tools":[
                {"name":"echo", "description":"Echo", "input_schema":schema, "output_schema":schema},
                {"name":"other", "description":"Other", "input_schema":schema, "output_schema":schema}
            ]
        })).unwrap()).unwrap()
    }
    fn handshake() -> Message {
        Message::Handshake {
            api_version: API_VERSION,
            mod_id: "org.example".into(),
        }
    }
    fn ready(slots: u32, now: Instant) -> Session {
        let mut s = Session::new(manifest(slots), 7);
        assert!(s.incoming(7, handshake(), now).unwrap().is_none());
        s
    }
    fn reply(request: &Message) -> Message {
        let Message::Request { id, tool, .. } = request else {
            panic!("request expected")
        };
        Message::Result {
            api_version: API_VERSION,
            id: id.clone(),
            tool: tool.clone(),
            outcome: Reply::Success { data: json!({}) },
        }
    }
    fn id(request: &Message) -> &str {
        let Message::Request { id, .. } = request else {
            panic!("request expected")
        };
        id
    }

    #[test]
    fn next_deadline_tracks_pending_work_without_polling_or_extending_cancel() {
        let now = Instant::now();
        let mut s = ready(2, now);
        assert_eq!(s.next_deadline(), None);
        let first = s.request(TOOL, json!({}), now).unwrap();
        let second = s
            .request(TOOL, json!({}), now + Duration::from_millis(2))
            .unwrap();
        assert_eq!(s.next_deadline(), Some(now + Duration::from_millis(10)));
        s.cancel(id(&first)).unwrap();
        assert_eq!(s.next_deadline(), Some(now + Duration::from_millis(10)));
        assert_eq!(s.expire(now + Duration::from_millis(10)).len(), 1);
        assert_eq!(s.next_deadline(), Some(now + Duration::from_millis(12)));
        s.incoming(7, reply(&second), now + Duration::from_millis(11))
            .unwrap();
        assert_eq!(s.next_deadline(), None);
        s.request(TOOL, json!({}), now + Duration::from_millis(12))
            .unwrap();
        s.close();
        assert_eq!(s.next_deadline(), None);
        let mut failed = ready(1, now);
        failed.request(TOOL, json!({}), now).unwrap();
        failed.fail();
        assert_eq!(failed.next_deadline(), None);
    }

    #[test]
    fn handshake_version_identity_and_direction() {
        let now = Instant::now();
        let mut s = Session::new(manifest(1), 7);
        assert!(s.request(TOOL, json!({}), now).is_err());
        for msg in [
            Message::Handshake {
                api_version: 2,
                mod_id: "org.example".into(),
            },
            Message::Handshake {
                api_version: 1,
                mod_id: "org.other".into(),
            },
        ] {
            assert!(s.incoming(7, msg, now).is_err());
            assert_eq!(s.state(), State::AwaitingHandshake);
        }
        s.incoming(7, handshake(), now).unwrap();
        assert!(s.incoming(7, handshake(), now).is_err());
        let req = s.request(TOOL, json!({}), now).unwrap();
        assert!(s.incoming(7, req.clone(), now).is_err());
        assert!(s
            .incoming(
                7,
                Message::Cancel {
                    api_version: 1,
                    id: id(&req).into()
                },
                now
            )
            .is_err());
        assert_eq!(s.in_flight(), 1);
    }

    #[test]
    fn invalid_results_do_not_consume_pending_and_terminal_is_once() {
        let now = Instant::now();
        let mut s = ready(1, now);
        let req = s.request(TOOL, json!({}), now).unwrap();
        let mut bad = reply(&req);
        if let Message::Result { tool, .. } = &mut bad {
            *tool = "mod_org_example__other".into();
        }
        assert!(s.incoming(7, bad, now).is_err());
        let mut bad = reply(&req);
        if let Message::Result { api_version, .. } = &mut bad {
            *api_version = 2;
        }
        assert!(s.incoming(7, bad, now).is_err());
        let mut bad = reply(&req);
        if let Message::Result { id, .. } = &mut bad {
            *id = "r999".into();
        }
        assert!(s.incoming(7, bad, now).is_err());
        assert_eq!(s.in_flight(), 1);
        let done = s.incoming(7, reply(&req), now).unwrap().unwrap();
        assert_eq!(done.id, id(&req));
        assert!(matches!(done.outcome, Outcome::Reply(_)));
        assert!(s.incoming(7, reply(&req), now).is_err());
        assert!(s.expire(now + Duration::from_secs(1)).is_empty());
        assert!(s.close().is_empty());
    }

    #[test]
    fn cancellation_keeps_slot_until_terminal() {
        let now = Instant::now();
        let mut s = ready(1, now);
        let req = s.request(TOOL, json!({}), now).unwrap();
        let cancel = s.cancel(id(&req)).unwrap();
        cancel.validate_reply_to(&req, &s.manifest).unwrap();
        assert!(s.cancel(id(&req)).is_err());
        assert!(s.request(TOOL, json!({}), now).is_err());
        assert_eq!(s.in_flight(), 1);
        assert!(matches!(
            s.incoming(7, reply(&req), now).unwrap().unwrap().outcome,
            Outcome::LocalCancelled
        ));
        assert!(s.incoming(7, reply(&req), now).is_err());
        assert_ne!(id(&s.request(TOOL, json!({}), now).unwrap()), id(&req));
    }

    #[test]
    fn deadlines_are_per_request_bounded_and_not_process_death() {
        let now = Instant::now();
        let mut s = ready(4, now);
        let first = s.request(TOOL, json!({}), now).unwrap();
        let second = s
            .request(TOOL, json!({}), now + Duration::from_millis(1))
            .unwrap();
        s.cancel(id(&second)).unwrap();
        for _ in 0..2 {
            s.request(TOOL, json!({}), now).unwrap();
        }
        assert!(s.request(TOOL, json!({}), now).is_err());
        assert!(s.expire(now + Duration::from_millis(9)).is_empty());
        let deadline = now + Duration::from_millis(10);
        assert!(s.incoming(7, reply(&first), deadline).is_err());
        assert_eq!(s.in_flight(), 4);
        let expired = s.expire(deadline);
        assert_eq!(expired.len(), 3);
        assert!(expired
            .iter()
            .all(|c| matches!(c.outcome, Outcome::TimedOut)));
        assert!(s.incoming(7, reply(&first), deadline).is_err());
        let cancelled = s.expire(deadline + Duration::from_millis(1));
        assert_eq!(cancelled.len(), 1);
        assert!(matches!(cancelled[0].outcome, Outcome::LocalCancelled));
        assert!(s.incoming(7, reply(&second), deadline).is_err());
        assert!(s.expire(deadline + Duration::from_secs(1)).is_empty());
        assert_eq!(s.state(), State::Ready);
    }

    #[test]
    fn restart_generation_rejects_old_connection_even_with_reused_id() {
        let now = Instant::now();
        let mut old = ready(1, now);
        let old_req = old.request(TOOL, json!({}), now).unwrap();
        let mut next = Session::new(manifest(1), 8);
        assert!(next.incoming(7, handshake(), now).is_err());
        assert!(next.incoming(8, reply(&old_req), now).is_err());
        next.incoming(8, handshake(), now).unwrap();
        let req = next.request(TOOL, json!({}), now).unwrap();
        assert_eq!(id(&req), id(&old_req));
        assert!(next.incoming(7, reply(&old_req), now).is_err());
        assert_eq!(next.in_flight(), 1);
        next.incoming(8, reply(&req), now).unwrap();
    }

    #[test]
    fn lifetime_ids_are_monotonic_and_bounded() {
        let now = Instant::now();
        let mut s = ready(1, now);
        assert!(s.request(TOOL, json!({"unexpected":true}), now).is_err());
        for expected in 1..=MAX_REQUESTS {
            let req = s.request(TOOL, json!({}), now).unwrap();
            assert_eq!(id(&req), format!("r{expected}"));
            let bytes = req.to_bytes(&s.manifest).unwrap();
            let parsed = Message::parse(&bytes, &s.manifest).unwrap();
            assert_eq!(id(&parsed), id(&req));
            s.incoming(7, reply(&parsed), now).unwrap();
        }
        assert_eq!(s.in_flight(), 0);
        assert!(s.request(TOOL, json!({}), now).is_err());
    }

    #[test]
    fn close_and_failure_drain_only_once_and_block_work() {
        let now = Instant::now();
        for failed in [false, true] {
            let mut s = ready(1, now);
            assert!(s.finish_close().is_err());
            let req = s.request(TOOL, json!({}), now).unwrap();
            let done = if failed { s.fail() } else { s.close() };
            assert_eq!(done.len(), 1);
            assert!(matches!(
                (&done[0].outcome, failed),
                (Outcome::Failed, true) | (Outcome::Closed, false)
            ));
            assert!(s.close().is_empty());
            assert!(s.incoming(7, reply(&req), now).is_err());
            assert!(s.incoming(7, handshake(), now).is_err());
            assert!(s.request(TOOL, json!({}), now).is_err());
            assert!(s.cancel(id(&req)).is_err());
            if !failed {
                assert_eq!(s.state(), State::Closing);
                s.finish_close().unwrap();
            }
            assert!(s.fail().is_empty());
            assert_eq!(
                s.state(),
                if failed { State::Failed } else { State::Closed }
            );
            assert!(s.expire(now + Duration::from_secs(1)).is_empty());
        }
    }
}
