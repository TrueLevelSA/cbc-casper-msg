use rayon::prelude::*;
use std::collections::HashSet;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};

use hashed::Hashed;
use justification::{Justification, LatestMsgsHonest, SenderState};
use traits::{Data, Estimate, Id, Sender};

/// A Casper Message, that can will be sent over the network
/// and used as a justification for a more recent message
pub trait CasperMsg: Hash + Clone + Eq + Sync + Send + Debug + Id + serde::Serialize {
    // To be implemented on concrete struct
    type Sender: Sender;
    type Estimate: Estimate<M = Self>;

    /// returns the validator who sent this message
    fn sender(&self) -> &Self::Sender;

    /// returns the estimate of this message
    fn estimate(&self) -> &Self::Estimate;

    /// returns the justification of this message
    fn justification<'z>(&'z self) -> &'z Justification<Self>;

    fn id(&self) -> &Self::ID;

    /// creates a new instance of this message
    fn new(
        sender: Self::Sender,
        justification: Justification<Self>,
        estimate: Self::Estimate,
        id: Option<Self::ID>,
    ) -> Self;

    // this function is used to clean up memory. when a msg is final, there's no
    // need to keep its justifications. when dropping its justification, all the
    // Msgs (Arc) which are referenced on the justification will get dropped
    // from memory
    // fn set_as_final(&mut self);

    // Following methods are actual implementations

    /// create a msg from newly received messages
    /// finalized_msg allows to shortcut the recursive checks
    fn from_msgs(
        sender: Self::Sender,
        mut new_msgs: Vec<&Self>,
        sender_state: &SenderState<Self>,
        external_data: Option<<Self::Estimate as Data>::Data>,
    ) -> Result<(Self, SenderState<Self>), &'static str> {
        // // TODO eventually comment out these lines, and FIXME tests
        // // check whether two messages from same sender
        // let mut senders = HashSet::new();
        // let dup_senders = new_msgs.iter().any(|msg| !senders.insert(msg.sender()));
        // assert!(!dup_senders, "A sender can only have one, and only one, latest message");

        // dedup by putting msgs into a hashset
        let new_msgs: HashSet<_> = new_msgs.drain(..).collect();
        let new_msgs_len = new_msgs.len();

        // update latest_msgs in sender_state with new_msgs
        let mut justification = Justification::new();
        let (success, sender_state) = justification.faulty_inserts(new_msgs, &sender_state);

        if !success && new_msgs_len > 0 {
            Err("None of the messages could be added to the state!")
        } else {
            let latest_msgs_honest = LatestMsgsHonest::from_latest_msgs(
                sender_state.latests_msgs(),
                sender_state.equivocators(),
            );

            let estimate =
                latest_msgs_honest.mk_estimate(&sender_state.senders_weights(), external_data);
            estimate.map(|e| (Self::new(sender, justification, e, None), sender_state))
        }
    }

    /// insanely expensive
    fn equivocates_indirect(
        &self,
        rhs: &Self,
        mut equivocators: HashSet<<Self as CasperMsg>::Sender>,
    ) -> (bool, HashSet<<Self as CasperMsg>::Sender>) {
        let is_equivocation = self.equivocates(rhs);
        let init = if is_equivocation {
            equivocators.insert(self.sender().clone());
            (true, equivocators)
        } else {
            (false, equivocators)
        };
        self.justification().iter().fold(
            init,
            |(acc_has_equivocations, acc_equivocators), self_prime| {
                // note the rotation between rhs and self, done because
                // descending only on self, thus rhs has to become self on the
                // recursion to get its justification visited
                let (has_equivocation, equivocators) =
                    rhs.equivocates_indirect(self_prime, acc_equivocators.clone());
                let acc_equivocators = acc_equivocators.union(&equivocators).cloned().collect();
                (acc_has_equivocations || has_equivocation, acc_equivocators)
            },
        )
    }

    /// Math definition of the equivocation
    fn equivocates(&self, rhs: &Self) -> bool {
        self != rhs && self.sender() == rhs.sender() && !rhs.depends(self) && !self.depends(rhs)
    }

    /// checks whether self depends on rhs
    /// returns true if rhs is somewhere in the justification of self
    fn depends(&self, rhs: &Self) -> bool {
        // although the recursion ends supposedly only at genesis message, the
        // trick is the following: it short-circuits while descending on the
        // dependency tree, if it finds a dependent message. when dealing with
        // honest validators, this would return true very fast. all the new
        // derived branches of the justification will be evaluated in parallel.
        // say if a msg is justified by 2 other msgs, then the 2 other msgs will
        // be processed on different threads. this applies recursively, so if
        // each of the 2 msgs have say 3 justifications, then each of the 2
        // threads will spawn 3 new threads to process each of the messages.
        // thus, highly parallelizable. when it shortcuts, because in one thread
        // a dependency was found, the function returns true and all the
        // computation on the other threads will be canceled.
        fn recurse<M: CasperMsg>(lhs: &M, rhs: &M, visited: Arc<RwLock<HashSet<M>>>) -> bool {
            let justification = lhs.justification();

            // Math definition of dependency
            justification.contains(rhs)
                || justification
                    .par_iter()
                    .filter(|lhs_prime| {
                        visited
                            .read()
                            .map(|v| !v.contains(lhs_prime))
                            .ok()
                            .unwrap_or(true)
                    })
                    .any(|lhs_prime| {
                        let visited_prime = visited.clone();
                        let _ = visited_prime
                            .write()
                            .map(|mut v| v.insert(lhs_prime.clone()))
                            .ok();
                        recurse(lhs_prime, rhs, visited_prime)
                    })
        }
        let visited = Arc::new(RwLock::new(HashSet::new()));
        recurse(self, rhs, visited)
    }

    /// checks whether ther is a circular dependency between self and rhs
    fn is_circular(&self, rhs: &Self) -> bool {
        rhs.depends(self) && self.depends(rhs)
    }
}

/// Mathematical definition of a casper message
#[derive(Clone, Default, Eq, PartialEq)]
struct ProtoMsg<E, S>
where
    E: Estimate<M = Message<E, S>>,
    S: Sender,
{
    estimate: E,
    sender: S,
    justification: Justification<Message<E, S>>,
}

/// Boxing of a ProtoMsg, that will implement the trait CasperMsg
#[derive(Eq, Clone, Default)]
pub struct Message<E, S>(Arc<ProtoMsg<E, S>>, Hashed)
where
    E: Estimate<M = Message<E, S>>,
    S: Sender;

/*
// TODO start we should make messages lazy. continue this after async-await is better
// documented

enum MsgStatus {
Unchecked,
Valid,
Invalid,
}

struct Message<E, S, D>
where
    E: Estimate,
    S: Sender,
{
    status: MsgStatus,
    msg: Future<Item = Message<E, S, D>, Error = Error>,
}
// TODO end
*/

// impl<E, S> From<ProtoMsg<E, S>> for Message<E, S>
// where
//     E: Estimate<M = Self>,
//     S: Sender,
// {
//     fn from(msg: ProtoMsg<E, S>) -> Self {
//         let id = msg.getid();
//         Message(Arc::new(msg), id)
//     }
// }

impl<E, S> Id for ProtoMsg<E, S>
where
    E: Estimate<M = Message<E, S>>,
    S: Sender,
{
    type ID = Hashed;
}

impl<E, S> Id for Message<E, S>
where
    E: Estimate<M = Self>,
    S: Sender,
{
    type ID = Hashed;
    fn getid(&self) -> Self::ID {
        self.1.clone()
    }
}

impl<E, S> serde::Serialize for ProtoMsg<E, S>
where
    E: Estimate<M = Message<E, S>>,
    S: Sender,
{
    fn serialize<T: serde::Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        use serde::ser::SerializeStruct;
        let mut msg = serializer.serialize_struct("Message", 3)?;
        let mut j: Vec<_> = self.justification.iter().map(Message::id).collect();
        let _ = j.sort_unstable();
        msg.serialize_field("sender", &self.sender)?;
        msg.serialize_field("estimate", &self.estimate)?;
        msg.serialize_field("justification", &j)?;
        msg.end()
    }
}

impl<E, S> serde::Serialize for Message<E, S>
where
    E: Estimate<M = Self>,
    S: Sender,
{
    fn serialize<T: serde::Serializer>(&self, serializer: T) -> Result<T::Ok, T::Error> {
        use serde::ser::SerializeStruct;
        let mut msg = serializer.serialize_struct("Message", 3)?;
        let j: Vec<_> = self.justification().iter().map(Self::id).collect();
        msg.serialize_field("sender", self.sender())?;
        msg.serialize_field("estimate", self.estimate())?;
        msg.serialize_field("justification", &j)?;
        msg.end()
    }
}

impl<E, S> CasperMsg for Message<E, S>
where
    E: Estimate<M = Self>,
    S: Sender,
{
    type Estimate = E;
    type Sender = S;

    fn sender(&self) -> &Self::Sender {
        &self.0.sender
    }

    fn estimate(&self) -> &Self::Estimate {
        &self.0.estimate
    }

    fn justification<'z>(&'z self) -> &'z Justification<Self> {
        &self.0.justification
    }

    fn id(&self) -> &<Self as Id>::ID {
        &self.1
    }

    fn new(
        sender: S,
        justification: Justification<Self>,
        estimate: E,
        id: Option<Self::ID>,
    ) -> Self {
        let proto = ProtoMsg {
            sender,
            justification,
            estimate,
        };
        let id = id.unwrap_or_else(|| proto.getid());
        Message(Arc::new(proto), id)
    }

    // fn set_as_final(&mut self) {
    //     let mut proto_msg = (**self.0).clone();
    //     proto_msg.justification = Justification::new();
    //     *self.0 = Arc::new(proto_msg);
    // }
}

impl<E, S> Hash for Message<E, S>
where
    E: Estimate<M = Self>,
    S: Sender,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id().hash(state)
    }
}

impl<E, S> PartialEq for Message<E, S>
where
    E: Estimate<M = Self>,
    S: Sender,
{
    fn eq(&self, rhs: &Self) -> bool {
        Arc::ptr_eq(&self.0, &rhs.0) || self.id() == rhs.id() // should make this line unnecessary
    }
}

impl<E, S> Debug for Message<E, S>
where
    E: Estimate<M = Self>,
    S: Sender,
{
    // Note: format used for rendering illustrative gifs from generative tests; modify with care
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "M{:?}({:?})",
            // "M{:?}({:?}) -> {:?}",
            self.sender(),
            self.estimate().clone(),
            // self.justification()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use example::vote_count::VoteCount;
    use justification::LatestMsgs;
    use senders_weight::SendersWeight;

    #[test]
    fn debug() {
        let v0 = VoteCount::create_vote_msg(0, false);
        println!("{:?}", v0);
    }

    #[test]
    fn msg_equality() {
        // v0 and v0_prime are equivocating messages (first child of a fork).
        let v0 = &VoteCount::create_vote_msg(0, false);
        let v1 = &VoteCount::create_vote_msg(1, true);
        let v0_prime = &VoteCount::create_vote_msg(0, true); // equivocating vote
        let v0_idem = &VoteCount::create_vote_msg(0, false);

        assert!(v0 == v0_idem, "v0 and v0_idem should be equal");
        assert!(v0 != v0_prime, "v0 and v0_prime should NOT be equal");
        assert!(v0 != v1, "v0 and v1 should NOT be equal");

        let senders_weights =
            SendersWeight::new([(0, 1.0), (1, 1.0), (2, 1.0)].iter().cloned().collect());

        let sender_state = SenderState::new(
            senders_weights,
            0.0,
            None,
            LatestMsgs::new(),
            0.0,
            HashSet::new(),
        );

        let mut j0 = Justification::new();
        let (success, _) = j0.faulty_inserts(vec![v0].iter().cloned().collect(), &sender_state);
        assert!(success);

        let external_data: Option<VoteCount> = None;
        let (m0, _) = &Message::from_msgs(0, vec![v0], &sender_state, external_data).unwrap();
        // let m0 = &Message::new(0, justification, estimate);

        let mut j1 = Justification::new();

        let (success, _) = j1.faulty_inserts(vec![v0].iter().cloned().collect(), &sender_state);
        assert!(success);

        let (success, _) = j1.faulty_inserts(vec![m0].iter().cloned().collect(), &sender_state);
        assert!(success);

        let (msg1, _) = Message::from_msgs(0, vec![v0], &sender_state, None).unwrap();
        let (msg2, _) = Message::from_msgs(0, vec![v0], &sender_state, None).unwrap();
        assert!(msg1 == msg2, "messages should be equal");

        let (msg3, _) = Message::from_msgs(0, vec![v0, m0], &sender_state, None).unwrap();
        assert!(msg1 != msg3, "msg1 should be different than msg3");
    }

    #[test]
    fn msg_depends() {
        let v0 = &VoteCount::create_vote_msg(0, false);
        let v0_prime = &VoteCount::create_vote_msg(0, true); // equivocating vote

        let senders_weights =
            SendersWeight::new([(0, 1.0), (1, 1.0), (2, 1.0)].iter().cloned().collect());

        let sender_state = SenderState::new(
            senders_weights,
            0.0,
            None,
            LatestMsgs::new(),
            0.0,
            HashSet::new(),
        );

        let mut j0 = Justification::new();
        let (success, _) = j0.faulty_inserts(vec![v0].iter().cloned().collect(), &sender_state);
        assert!(success);

        let (m0, _) = &Message::from_msgs(0, vec![v0], &sender_state, None).unwrap();

        assert!(
            !v0.depends(v0_prime),
            "v0 does NOT depends on v0_prime as they are equivocating"
        );
        assert!(
            !m0.depends(m0),
            "m0 does NOT depends on itself directly, by our impl choice"
        );
        assert!(!m0.depends(v0_prime), "m0 depends on v0 directly");
        assert!(m0.depends(v0), "m0 depends on v0 directly");

        let mut j0 = Justification::new();
        let (success, _) = j0.faulty_inserts([v0].iter().cloned().collect(), &sender_state);
        assert!(success);

        let (m0, _) = &Message::from_msgs(0, vec![v0], &sender_state, None).unwrap();

        let mut j1 = Justification::new();
        let (success, _) = j1.faulty_inserts([v0].iter().cloned().collect(), &sender_state);
        assert!(success);

        let (success, _) = j1.faulty_inserts([m0].iter().cloned().collect(), &sender_state);
        assert!(success);

        let (m1, _) = &Message::from_msgs(0, vec![v0, m0], &sender_state, None).unwrap();

        assert!(m1.depends(m0), "m1 DOES depent on m0");
        assert!(!m0.depends(m1), "but m0 does NOT depend on m1");
        assert!(m1.depends(v0), "m1 depends on v0 through m0");
    }

    #[test]
    fn msg_equivocates() {
        let v0 = &VoteCount::create_vote_msg(0, false);
        let v0_prime = &VoteCount::create_vote_msg(0, true); // equivocating vote
        let v1 = &VoteCount::create_vote_msg(1, true);

        let senders_weights =
            SendersWeight::new([(0, 1.0), (1, 1.0), (2, 1.0)].iter().cloned().collect());
        let sender_state = SenderState::new(
            senders_weights,
            0.0,
            None,
            LatestMsgs::new(),
            0.0,
            HashSet::new(),
        );

        let mut j0 = Justification::new();
        let (success, _) = j0.faulty_inserts(vec![v0].iter().cloned().collect(), &sender_state);
        assert!(success);

        let (m0, _) = &Message::from_msgs(0, vec![v0], &sender_state, None).unwrap();

        let (m1, _) = &Message::from_msgs(1, vec![v0], &sender_state, None).unwrap();
        assert!(!v0.equivocates(v0), "should be all good");
        assert!(!v1.equivocates(m0), "should be all good");
        assert!(!m0.equivocates(v1), "should be all good");
        assert!(v0.equivocates(v0_prime), "should be a direct equivocation");

        assert!(
            m0.equivocates(v0_prime),
            "should be an indirect equivocation, equivocates to m0 through v0"
        );
        assert!(
            m1.equivocates_indirect(v0_prime, HashSet::new()).0,
            "should be an indirect equivocation, equivocates to m0 through v0"
        );
    }

    // #[test]
    // fn set_as_final() {
    //     let sender0 = 0;
    //     let sender1 = 1;
    //     let senders_weights = SendersWeight::new(
    //         [(sender0, 1.0), (sender1, 1.0)].iter().cloned().collect(),
    //     );
    //     let sender_state = SenderState::new(
    //         senders_weights.clone(),
    //         0.0,
    //         None,
    //         LatestMsgs::new(),
    //         0.0,
    //         HashSet::new(),
    //     );
    //     let senders = &senders_weights.senders().unwrap();

    //     // sender0        v0---m0        m2---
    //     // sender1               \--m1--/
    //     let v0 = &VoteCount::create_vote_msg(sender1, false);
    //     let safe_msgs = v0.get_msg_for_proposition(senders);
    //     assert_eq!(safe_msgs.len(), 0, "only sender0 saw v0");

    //     let (m0, sender_state) = &mut Message::from_msgs(
    //         sender0,
    //         vec![v0],
    //         None,
    //         &sender_state,
    //         None as Option<VoteCount>,
    //     ).unwrap();

    //     let (m1, sender_state) = &Message::from_msgs(
    //         sender1,
    //         vec![m0],
    //         None,
    //         &sender_state,
    //         None as Option<VoteCount>,
    //     ).unwrap();

    //     let (m2, _) = &Message::from_msgs(
    //         sender0,
    //         vec![m1],
    //         None,
    //         &sender_state,
    //         None as Option<VoteCount>,
    //     ).unwrap();

    //     let safe_msgs = m2.get_msg_for_proposition(senders);

    //     assert!(safe_msgs.len() == 1);
    //     println!("------------");
    //     println!("message before trimmed by set_as_final\n {:?}", m0);
    //     m0.set_as_final();
    //     println!("message after\n {:?}", m0);
    //     println!("------------");
    // }
}
