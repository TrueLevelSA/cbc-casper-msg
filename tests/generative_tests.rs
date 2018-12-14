#[macro_use]
extern crate proptest;
extern crate casper;
extern crate rand;

mod common;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::iter;
use std::iter::FromIterator;

use proptest::prelude::*;

use proptest::strategy::ValueTree;
use proptest::test_runner::Config;
use proptest::test_runner::TestRunner;
use rand::seq::SliceRandom;
use rand::thread_rng;

use casper::blockchain::{Block, BlockMsg, ProtoBlock};
use casper::justification::{Justification, LatestMsgs, LatestMsgsHonest, SenderState};
use casper::message::*;
use casper::senders_weight::SendersWeight;
use casper::traits::{Data, Estimate};

use common::binary::BoolWrapper;
use common::integer::IntegerWrapper;
use common::vote_count::VoteCount;

fn add_message<'z, M>(
    state: &'z mut HashMap<M::Sender, SenderState<M>>,
    sender: M::Sender,
    recipients: HashSet<M::Sender>,
    data: Option<<M::Estimate as Data>::Data>,
) -> &'z HashMap<M::Sender, SenderState<M>>
where
    M: CasperMsg,
{
    let latest: HashSet<M> = state[&sender]
        .get_latest_msgs()
        .iter()
        .fold(HashSet::new(), |acc, (_, lms)| {
            acc.union(&lms).cloned().collect()
        });
    let latest_delta = match state[&sender].get_my_last_msg() {
        Some(m) => latest
            .iter()
            .filter(|lm| !m.get_justification().contains(lm))
            .cloned()
            .collect(),
        None => latest,
    };
    let (m, sender_state) = M::from_msgs(
        sender.clone(),
        latest_delta.iter().collect(),
        None,
        &state[&sender],
        data.clone().map(|d| d.into()),
    )
    .unwrap();

    state.insert(sender.clone(), sender_state);
    recipients.iter().for_each(|recipient| {
        state
            .get_mut(&recipient)
            .unwrap()
            .get_latest_msgs_as_mut()
            .update(&m);
    });
    state
}

fn round_robin(val: &mut Vec<u32>) -> BoxedStrategy<u32> {
    let v = val.pop().unwrap();
    val.insert(0, v);
    Just(v).boxed()
}

fn arbitrary_in_set(val: &mut Vec<u32>) -> BoxedStrategy<u32> {
    prop::sample::select(val.clone()).boxed()
}

fn all_receivers(val: &Vec<u32>) -> BoxedStrategy<HashSet<u32>> {
    let v = HashSet::from_iter(val.iter().cloned());
    Just(v).boxed()
}

fn some_receivers(val: &Vec<u32>) -> BoxedStrategy<HashSet<u32>> {
    prop::collection::hash_set(prop::sample::select(val.clone()), 0..(val.len() + 1)).boxed()
}

fn message_event<M>(
    state: HashMap<M::Sender, SenderState<M>>,
    sender_strategy: BoxedStrategy<M::Sender>,
    receiver_strategy: BoxedStrategy<HashSet<M::Sender>>,
) -> BoxedStrategy<HashMap<M::Sender, SenderState<M>>>
where
    M: 'static + CasperMsg,
    <<M as CasperMsg>::Estimate as Data>::Data: From<<M as CasperMsg>::Sender>,
{
    (sender_strategy, receiver_strategy, Just(state))
        .prop_map(|(sender, mut receivers, mut state)| {
            if !receivers.contains(&sender) {
                receivers.insert(sender.clone());
            }
            add_message(&mut state, sender.clone(), receivers, Some(sender.into())).clone()
        })
        .boxed()
}

fn full_consensus<M>(state: &HashMap<M::Sender, SenderState<M>>) -> bool
where
    M: CasperMsg,
{
    let m: HashSet<_> = state
        .iter()
        .map(|(_sender, sender_state)| {
            let latest_honest_msgs =
                LatestMsgsHonest::from_latest_msgs(sender_state.get_latest_msgs(), &HashSet::new());
            latest_honest_msgs.mk_estimate(None, sender_state.get_senders_weights(), None)
        })
        .collect();
    println!("{:?}", m);
    m.len() == 1
}

fn safety_oracle(state: &HashMap<u32, SenderState<BlockMsg>>) -> bool {
    let safety_oracle_detected: HashSet<bool> = state
        .iter()
        .map(|(_, sender_state)| {
            let latest_honest_msgs =
                LatestMsgsHonest::from_latest_msgs(sender_state.get_latest_msgs(), &HashSet::new());
            let genesis_block = Block::from(ProtoBlock::new(None, 0));
            let safety_threshold = (sender_state.get_senders_weights().sum_all_weights()) / 2.0;
            Block::safety_oracles(
                genesis_block,
                &latest_honest_msgs,
                &HashSet::new(),
                safety_threshold,
                sender_state.get_senders_weights(),
            ) != HashSet::new()
        })
        .collect();
    safety_oracle_detected.contains(&true)
}

fn clique_collection(state: HashMap<u32, SenderState<BlockMsg>>) -> Vec<Vec<Vec<u32>>> {
    state
        .iter()
        .map(|(_, sender_state)| {
            let genesis_block = Block::from(ProtoBlock::new(None, 0));
            let latest_honest_msgs =
                LatestMsgsHonest::from_latest_msgs(sender_state.get_latest_msgs(), &HashSet::new());
            let safety_oracles = Block::safety_oracles(
                genesis_block,
                &latest_honest_msgs,
                &HashSet::new(),
                // cliques, not safety oracles, because our threshold is 0
                0.0,
                sender_state.get_senders_weights(),
            );
            let safety_oracles_vec_of_btrees: Vec<BTreeSet<u32>> =
                Vec::from_iter(safety_oracles.iter().cloned());
            let safety_oracles_vec_of_vecs: Vec<Vec<u32>> = safety_oracles_vec_of_btrees
                .iter()
                .map(|btree| Vec::from_iter(btree.iter().cloned()))
                .collect();
            safety_oracles_vec_of_vecs
        })
        .collect()
}

fn chain<E: 'static, F: 'static, G: 'static, H: 'static>(
    consensus_value_strategy: BoxedStrategy<E>,
    validator_max_count: usize,
    message_producer_strategy: F,
    message_receiver_strategy: G,
    consensus_satisfied: H,
) -> BoxedStrategy<Vec<HashMap<u32, SenderState<Message<E, u32>>>>>
where
    E: Estimate<M = Message<E, u32>> + From<u32>,
    F: Fn(&mut Vec<u32>) -> BoxedStrategy<u32>,
    G: Fn(&Vec<u32>) -> BoxedStrategy<HashSet<u32>>,
    H: Fn(&HashMap<u32, SenderState<Message<E, u32>>>) -> bool,
{
    (prop::sample::select((1..validator_max_count).collect::<Vec<usize>>()))
        .prop_flat_map(move |validators| {
            (prop::collection::vec(consensus_value_strategy.clone(), validators))
        })
        .prop_map(move |votes| {
            let mut state = HashMap::new();
            let validators: Vec<u32> = (0..votes.len() as u32).collect();

            let weights: Vec<f64> = iter::repeat(1.0).take(votes.len() as usize).collect();

            let senders_weights = SendersWeight::new(
                validators
                    .iter()
                    .cloned()
                    .zip(weights.iter().cloned())
                    .collect(),
            );

            validators.iter().for_each(|validator| {
                let mut j = Justification::new();
                let m = Message::new(
                    *validator,
                    j.clone(),
                    votes[*validator as usize].clone(),
                    None,
                );
                j.insert(m.clone());
                state.insert(
                    *validator,
                    SenderState::new(
                        senders_weights.clone(),
                        0.0,
                        Some(m),
                        LatestMsgs::from(&j),
                        0.0,
                        HashSet::new(),
                    ),
                );
            });

            let mut runner = TestRunner::default();
            let mut senders = validators.clone();
            let chain = iter::repeat_with(|| {
                let sender_strategy = message_producer_strategy(&mut senders);
                let receiver_strategy = message_receiver_strategy(&senders);
                state = message_event(state.clone(), sender_strategy, receiver_strategy)
                    .new_value(&mut runner)
                    .unwrap()
                    .current();
                state.clone()
            });
            let mut have_consensus = false;
            Vec::from_iter(chain.take_while(|state| {
                if have_consensus {
                    false
                } else {
                    if consensus_satisfied(state) {
                        have_consensus = true
                    }
                    true
                }
            }))
        })
        .boxed()
}

fn arbitrary_blockchain() -> BoxedStrategy<Block> {
    let genesis_block = Block::from(ProtoBlock::new(None, 0));
    Just(genesis_block).boxed()
}

proptest! {
    #![proptest_config(Config::with_cases(100))]
    #[test]
    fn blockchain(ref chain in chain(arbitrary_blockchain(), 6, round_robin, some_receivers, safety_oracle)) {
        // total messages until unilateral consensus
        println!("new chain");
        chain.iter().for_each(|state| {println!("{{lms: {:?},", state.iter().map(|(_, sender_state)|
                                                                                 sender_state.get_latest_msgs()).collect::<Vec<_>>());
                                       println!("sendercount: {:?},", state.keys().len());
                                       print!("clqs: ");
                                       println!("{:?}}},", clique_collection(state.clone()));
        });
    }
}

proptest! {
    #![proptest_config(Config::with_cases(30))]
    #[test]
    fn round_robin_vote_count(ref chain in chain(VoteCount::arbitrary(), 15, round_robin, all_receivers, full_consensus)) {
        assert_eq!(chain.last().unwrap_or(&HashMap::new()).keys().len(),
                   if chain.len() > 0 {chain.len()} else {0},
                   "round robin with n validators should converge in n messages")
    }
}

prop_compose! {
    fn boolwrapper_gen()
        (boolean in prop::bool::ANY) -> BoolWrapper {
            BoolWrapper::new(boolean)
        }
}

prop_compose! {
    fn integerwrapper_gen()
        (int in prop::num::u32::ANY) -> IntegerWrapper {
            IntegerWrapper::new(int)
        }
}

proptest! {
    #![proptest_config(Config::with_cases(30))]
    #[test]
    fn round_robin_binary(ref chain in chain(boolwrapper_gen(), 15, round_robin, all_receivers, full_consensus)) {
        assert!(chain.last().unwrap_or(&HashMap::new()).keys().len() >=
                chain.len(),
                "round robin with n validators should converge in at most n messages")
    }
}

proptest! {
    #![proptest_config(Config::with_cases(10))]
    #[test]
    fn round_robin_integer(ref chain in chain(integerwrapper_gen(), 2000, round_robin, all_receivers, full_consensus)) {
        // total messages until unilateral consensus
        println!("{} validators -> {:?} message(s)",
                 match chain.last().unwrap_or(&HashMap::new()).keys().len().to_string().as_ref()
                 {"0" => "Unknown",
                  x => x},
                 chain.len());
        assert!(chain.last().unwrap_or(&HashMap::new()).keys().len() >=
                chain.len(),
                "round robin with n validators should converge in at most n messages")
    }
}

proptest! {
    #![proptest_config(Config::with_cases(1))]
    #[test]
    fn arbitrary_messenger_vote_count(ref chain in chain(VoteCount::arbitrary(), 8, arbitrary_in_set, some_receivers, full_consensus)) {
        // total messages until unilateral consensus
        println!("{} validators -> {:?} message(s)",
                 match chain.last().unwrap_or(&HashMap::new()).keys().len().to_string().as_ref()
                 {"0" => "Unknown",
                  x => x},
                 chain.len());
    }
}

proptest! {
    #![proptest_config(Config::with_cases(1))]
    #[test]
    fn arbitrary_messenger_binary(ref chain in chain(boolwrapper_gen(), 100, arbitrary_in_set, some_receivers, full_consensus)) {
        // total messages until unilateral consensus
        println!("{} validators -> {:?} message(s)",
                 match chain.last().unwrap_or(&HashMap::new()).keys().len().to_string().as_ref()
                 {"0" => "Unknown",
                  x => x},
                 chain.len());
    }
}

proptest! {
    #![proptest_config(Config::with_cases(1))]
    #[test]
    fn arbitrary_messenger_integer(ref chain in chain(integerwrapper_gen(), 50, arbitrary_in_set, some_receivers, full_consensus)) {
        // total messages until unilateral consensus
        println!("{} validators -> {:?} message(s)",
                 match chain.last().unwrap_or(&HashMap::new()).keys().len().to_string().as_ref()
                 {"0" => "Unknown",
                  x => x},
                 chain.len());
    }
}

prop_compose! {
    fn votes(senders: usize, equivocations: usize)
        (votes in prop::collection::vec(prop::bool::weighted(0.3), senders as usize),
         equivocations in Just(equivocations),
         senders in Just(senders))
         -> (Vec<Message<VoteCount, u32>>, HashSet<u32>, usize)
    {
        let mut validators: Vec<u32> = (0..senders as u32).collect();
        validators.shuffle(&mut thread_rng());
        let equivocators: HashSet<u32> = HashSet::from_iter(validators[0..equivocations].iter().cloned());

        let mut messages = vec![];
        votes
            .iter()
            .enumerate()
            .for_each(|(sender, vote)|
                      {messages.push(VoteCount::create_vote_msg(sender as u32, vote.clone()))});
        equivocators
            .iter()
            .for_each(|equivocator|
                      {let vote = !votes[*equivocator as usize];
                       messages.push(VoteCount::create_vote_msg(*equivocator as u32, vote))});
        (messages, equivocators, senders)
    }
}

proptest! {
    #![proptest_config(Config::with_cases(1000))]
    #[test]
    fn detect_equivocation(ref vote_tuple in votes(5, 5)) {
        let (messages, equivocators, nodes) = vote_tuple;
        let nodes = nodes.clone();
        let senders: Vec<u32> = (0..nodes as u32).collect();
        let weights: Vec<f64> =
            iter::repeat(1.0).take(nodes as usize).collect();
        let senders_weights = SendersWeight::new(
            senders
                .iter()
                .cloned()
                .zip(weights.iter().cloned())
                .collect(),
        );
        let sender_state = SenderState::new(
            senders_weights.clone(),
            0.0,
            None,
            LatestMsgs::new(),
            0.0,
            HashSet::new(),
        );

        // here, only take one equivocation
        let single_equivocation: Vec<_> = messages[..nodes+1].iter().map(|message| message).collect();
        let equivocator = messages[nodes].get_sender();
        let (m0, _) =
            &Message::from_msgs(0, single_equivocation.clone(), None, &sender_state, None)
            .unwrap();
        let equivocations: Vec<_> = single_equivocation.iter().filter(|message| message.equivocates(&m0)).collect();
        assert!(if *equivocator == 0 {equivocations.len() == 1} else {equivocations.len() == 0}, "should detect sender 0 equivocating if sender 0 equivocates");
        // the following commented test should fail
        // assert_eq!(equivocations.len(), 1, "should detect sender 0 equivocating if sender 0 equivocates");

        let (m0, _) =
            &Message::from_msgs(0, messages.iter().map(|message| message).collect(), None, &sender_state, None)
            .unwrap();
        let equivocations: Vec<_> = messages.iter().filter(|message| message.equivocates(&m0)).collect();
        assert_eq!(equivocations.len(), 1, "should detect sender 0 equivocating if sender 0 equivocates");

        let sender_state = SenderState::new(
            senders_weights,
            0.0,
            None,
            LatestMsgs::new(),
            equivocators.len() as f64,
            HashSet::new(),
        );
        let (m0, _) =
            &Message::from_msgs(0, messages.iter().map(|message| message).collect(), None, &sender_state, None)
            .unwrap();
        let equivocations: Vec<_> = messages.iter().filter(|message| message.equivocates(&m0)).collect();
        assert_eq!(equivocations.len(), 0, "equivocation absorbed in threshold");
    }
}
