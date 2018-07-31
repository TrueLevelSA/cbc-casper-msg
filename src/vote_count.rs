use std::collections::{HashSet};
use std::ops::{Add};
use std::fmt::{Debug, Formatter, Result};

use traits::{Zero, Estimate, Sender, Data};
use message::{Message, AbstractMsg};
use justification::{Justification, Weights};

#[derive(Clone, Eq, Ord, PartialOrd, PartialEq, Hash, Default)]
pub struct VoteCount {
    yes: u32,
    no: u32,
}

impl Zero<VoteCount> for VoteCount {
    const ZERO: Self = Self { yes: 0, no: 0 };
}

impl Add for VoteCount {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        VoteCount {
            yes: self.yes + other.yes,
            no: self.no + other.no,
        }
    }
}

impl Debug for VoteCount {
    fn fmt(&self, f: &mut Formatter) -> Result {
        write!(f, "y{:?}/n{:?}", self.yes, self.no)
    }
}

impl VoteCount {
    // makes sure nobody adds more than one vote to their unjustified VoteCount
    // object. if they did, their vote is invalid and will be ignored
    fn is_valid_vote(vote: &Option<Self>) -> bool {
        // these two are the only allowed votes (unjustified msgs)
        match vote {
            Some(VoteCount { yes: 1, no: 0 }) => true,
            Some(VoteCount { yes: 0, no: 1 }) => true,
            _ => false,
        }
    }

    // used to create an equivocation vote
    fn toggle_vote(vote: &Option<Self>) -> Option<Self> {
        // these two are the only allowed votes (unjustified msgs)
        match vote {
            Some(VoteCount { yes: 1, no: 0 }) =>
                Some(VoteCount { yes: 0, no: 1 }),
            Some(VoteCount { yes: 0, no: 1 }) =>
                Some(VoteCount { yes: 1, no: 0 }),
            _ => None,
        }
    }

    pub fn create_vote_msg(sender: u32, vote: bool) -> Message<Self, u32> {
        let justification = Justification::new();
        let estimate = match vote {
            true => Some(VoteCount { yes: 1, no: 0 }),
            false => Some(VoteCount { yes: 0, no: 1 }),
        };

        Message::new(sender, justification, estimate)
    }

    fn get_vote_msgs(
        msg: &Message<Self, Voter>,
    ) -> HashSet<Message<Self, Voter>> {
        fn recursor(
            msg: &Message<VoteCount, Voter>,
            acc: HashSet<Message<VoteCount, Voter>>,
        ) -> HashSet<Message<VoteCount, Voter>> {
            msg.get_justification()
                .iter()
                .fold(acc, |mut acc_prime, m| {
                    match m.get_justification().len() {
                        0 => {
                            // vote found, vote is a message with 0 justification
                            let estimate = m
                                .get_estimate()
                                .and_then(|estimate| Some(estimate.clone()));
                            if VoteCount::is_valid_vote(&estimate) {
                                let equivocation = Message::new(
                                    m.get_sender().clone(),
                                    m.get_justification().clone(),
                                    VoteCount::toggle_vote(&estimate),
                                );
                                // search for the equivocation of the current msg
                                match acc_prime.get(&equivocation) {
                                    // remove the equivoted vote, none of the pair
                                    // will stay on the set
                                    Some(_) => acc_prime.remove(&equivocation),
                                    // add the vote
                                    None => acc_prime.insert((*m).clone()),
                                };
                            }
                            acc_prime // returns it
                        },
                        _ => recursor(&m, acc_prime),
                    }
                })
        }
        // start recursion
        recursor(msg, HashSet::new())
    }
}

type Voter = u32;
impl Sender for Voter {}
impl Data for VoteCount {}

impl Estimate for VoteCount {
    // the estimator just counts votes, which in this case are the unjustified
    // msgs
    type M = Message<Self, Voter>;
    type Sender = Voter;

    // Data could be anything, as it will not be used, will just pass None to
    // mk_estimate, as it takes an Option
    type Data = Self;

    fn mk_estimate(
        latest_msgs: &Justification<Self::M>,
        _weights: &Weights<Voter>, // all voters have same weight
        _external_data: Option<Self::Data>,
    ) -> Option<Self> {
        // stub msg w/ no estimate and no valid sender that will be dropped on
        // the pattern matching below
        let msg = Message::new(
            ::std::u32::MAX, // sender,
            latest_msgs.clone(),
            None, // estimate, will be droped on the pattern matching below
        );
        // the estimates are actually the original votes of each of the voters /
        // validators
        let votes = Self::get_vote_msgs(&msg);
        let res = votes.iter().fold(Self::ZERO, |acc, vote| {
            match vote.get_estimate() {
                Some(estimate) => acc + estimate.clone(),
                None => acc, // skip counting
            }
        });
        Some(res)
    }
}

mod count_votes {
    use super::*;
    #[test]
    fn count_votes() {
        use justification::{Weights};
        use senders_weight::{SendersWeight};
        let senders_weights = SendersWeight::new(
            [(0, 1.0), (1, 1.0), (2, 1.0)].iter().cloned().collect(),
        );
        let v0 = &VoteCount::create_vote_msg(0, false);
        let v0_prime = &VoteCount::create_vote_msg(0, true); // equivocating vote
        let v1 = &VoteCount::create_vote_msg(1, true);
        let mut j0 = Justification::new();
        let weights = Weights::new(senders_weights, 0.0, 2.0);
        assert!(j0.faulty_insert(vec![v0], &weights).success);
        let (m0, _) = &Message::from_msgs(0, vec![v0], &weights, None);
        let mut j1 = Justification::new();
        assert!(j1.faulty_insert(vec![v1], &weights).success);
        assert!(j1.faulty_insert(vec![m0], &weights).success);

        let (m1, _) = &Message::from_msgs(1, vec![v1, m0], &weights, None);
        assert_eq!(
            Message::get_estimate(m1).clone().unwrap(),
            VoteCount { yes: 1, no: 1 },
            "should have 1 yes, and 1 no vote, found {:?}",
            Message::get_estimate(m1).clone().unwrap(),
        );

        assert!(j1.faulty_insert(vec![v0_prime], &weights).success);
        let (m1_prime, _) = &Message::from_msgs(1, vec![v1, m0, v0_prime], &weights, None);
        assert_eq!(
            Message::get_estimate(m1_prime).clone().unwrap(),
            VoteCount { yes: 1, no: 0 },
            "should have 1 yes, and 0 no vote, found {:?}, the equivocation vote should cancels out the normal vote",
            Message::get_estimate(&m1_prime).clone().unwrap(),)
    }
}