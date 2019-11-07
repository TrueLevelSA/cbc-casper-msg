// Core CBC Rust Library
// Copyright (C) 2018  Coordination Technology Ltd.
// Authors: pZ4 <pz4@protonmail.ch>,
//          Lederstrumpf,
//          h4sh3d <h4sh3d@truelevel.io>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! # Later Messages
//!
//! If message *A* is in the justification of message *B*, then message *B* is **later** than
//! message *A*.
//!
//! # Estimator Function
//!
//! The **estimator function takes the justification** (which is a set of messages) as input, and
//! **returns the set of consensus values** that are “justified” by the input.  For example, in an
//! integer consensus setting, the estimator will return integer values. In a blockchain setting,
//! the the estimator will return blocks which can be added on top of the current tip detected from
//! the blocks in the messages in the inputted justification.
//!
//! Source: [Casper CBC, Simplified!](https://medium.com/@aditya.asgaonkar/casper-cbc-simplified-2370922f9aa6),
//! by Aditya Asgaonkar.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::{Debug, Formatter};

use rayon::iter::IntoParallelRefIterator;

use crate::estimator::Estimator;
use crate::message::Message;
use crate::util::weight::{WeightUnit, Zero};
use crate::validator;

/// Struct that holds the set of the `message::Trait` that justify the current message. Works like
/// a `vec`.
#[derive(Eq, PartialEq, Clone, Hash)]
pub struct Justification<E: Estimator>(Vec<Message<E>>);

impl<E: Estimator> Justification<E> {
    /// Create an empty justification.
    pub fn empty() -> Self {
        Justification(Vec::new())
    }

    /// Creates and return a new justification instance from a vector of `message::Trait` and
    /// mutate the given `validator::State` with the updated state
    pub fn from_msgs<U: WeightUnit>(
        messages: Vec<Message<E>>,
        state: &mut validator::State<E, U>,
    ) -> Self {
        let mut justification = Justification::empty();
        let messages: HashSet<_> = messages.iter().collect();
        justification.faulty_inserts(&messages, state);
        justification
    }

    pub fn iter(&self) -> std::slice::Iter<Message<E>> {
        self.0.iter()
    }

    pub fn par_iter(&self) -> rayon::slice::Iter<Message<E>> {
        self.0.par_iter()
    }

    pub fn contains(&self, msg: &Message<E>) -> bool {
        self.0.contains(msg)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn insert(&mut self, msg: Message<E>) -> bool {
        if self.contains(&msg) {
            false
        } else {
            self.0.push(msg);
            true
        }
    }

    /// Run the estimator on the justification given the set of equivocators and validators' weights.
    pub fn mk_estimate<U: WeightUnit>(
        &self,
        equivocators: &HashSet<<E as Estimator>::ValidatorName>,
        validators_weights: &validator::Weights<<E as Estimator>::ValidatorName, U>,
    ) -> Result<E, E::Error> {
        let latest_msgs = LatestMsgs::from(self);
        let latest_msgs_honest = LatestMsgsHonest::from_latest_msgs(&latest_msgs, equivocators);
        Estimator::estimate(&latest_msgs_honest, validators_weights)
    }

    /// Insert messages to the justification, accepting up to the threshold faults by weight.
    /// Returns a HashSet of messages that got successfully included in the justification.
    pub fn faulty_inserts<'a, U: WeightUnit>(
        &mut self,
        msgs: &HashSet<&'a Message<E>>,
        state: &mut validator::State<E, U>,
    ) -> HashSet<&'a Message<E>> {
        let msgs = state.sort_by_faultweight(msgs);
        // do the actual insertions to the state
        msgs.into_iter()
            .filter(|msg| self.faulty_insert(msg, state))
            .collect()
    }

    /// This function makes no assumption on how to treat the equivocator. it adds the msg to the
    /// justification only if it will not cross the fault tolerance threshold.
    pub fn faulty_insert<U: WeightUnit>(
        &mut self,
        msg: &Message<E>,
        state: &mut validator::State<E, U>,
    ) -> bool {
        let is_equivocation = state.latest_msgs.equivocate(msg);

        let sender = msg.sender();
        let validator_weight = state
            .validators_weights
            .weight(sender)
            .unwrap_or(U::INFINITY);

        let already_in_equivocators = state.equivocators.contains(sender);

        match (is_equivocation, already_in_equivocators) {
            // if it's already equivocating and listed as such, or not equivocating at all, an
            // insertion can be done without more checks
            (false, _) | (true, true) => {
                let success = self.insert(msg.clone());
                if success {
                    state.latest_msgs.update(msg);
                }
                success
            }
            // in the other case, we have to check that the threshold is not reached
            (true, false) => {
                if validator_weight + state.state_fault_weight <= state.thr {
                    let success = self.insert(msg.clone());
                    if success {
                        state.latest_msgs.update(msg);
                        if state.equivocators.insert(sender.clone()) {
                            state.state_fault_weight += validator_weight;
                        }
                    }
                    success
                } else {
                    false
                }
            }
        }
    }

    /// This function sets the weight of the equivocator to zero right away (returned in
    /// `validator::State`) and add his message to the state, since now his equivocation doesnt count
    /// to the state fault weight anymore
    pub fn faulty_insert_with_slash<'a, U: WeightUnit>(
        &mut self,
        msg: &Message<E>,
        state: &'a mut validator::State<E, U>,
    ) -> Result<bool, validator::Error<'a, HashMap<<E as Estimator>::ValidatorName, U>>> {
        let is_equivocation = state.latest_msgs.equivocate(msg);
        if is_equivocation {
            let sender = msg.sender();
            state.equivocators.insert(sender.clone());
            state
                .validators_weights
                .insert(sender.clone(), <U as Zero<U>>::ZERO)?;
        }
        state.latest_msgs.update(msg);
        let success = self.insert(msg.clone());
        Ok(success)
    }
}

impl<E: Estimator> Debug for Justification<E> {
    fn fmt(&self, f: &mut Formatter) -> ::std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}

/// Mapping between validators and their latests messages. Latest messages from a validator are all
/// their messages that are not in the dependency of another of their messages.
#[derive(Eq, PartialEq, Clone, Debug)]
pub struct LatestMsgs<E: Estimator>(HashMap<<E as Estimator>::ValidatorName, HashSet<Message<E>>>);

impl<E: Estimator> LatestMsgs<E> {
    /// Create an empty set of latest messages.
    pub fn empty() -> Self {
        LatestMsgs(HashMap::new())
    }

    /// Insert a new set of messages for a sender.
    pub fn insert(
        &mut self,
        k: <E as Estimator>::ValidatorName,
        v: HashSet<Message<E>>,
    ) -> Option<HashSet<Message<E>>> {
        self.0.insert(k, v)
    }

    /// Checks whether a sender is already contained in the map.
    pub fn contains_key(&self, k: &<E as Estimator>::ValidatorName) -> bool {
        self.0.contains_key(k)
    }

    /// Get a set of messages sent by the sender.
    pub fn get(&self, k: &<E as Estimator>::ValidatorName) -> Option<&HashSet<Message<E>>> {
        self.0.get(k)
    }

    /// Get a mutable set of messages sent by the sender.
    pub fn get_mut(
        &mut self,
        k: &<E as Estimator>::ValidatorName,
    ) -> Option<&mut HashSet<Message<E>>> {
        self.0.get_mut(k)
    }

    /// Get an iterator on the set.
    pub fn iter(
        &self,
    ) -> std::collections::hash_map::Iter<<E as Estimator>::ValidatorName, HashSet<Message<E>>>
    {
        self.0.iter()
    }

    /// Get the set size.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Get the set keys, i.e. the senders.
    pub fn keys(
        &self,
    ) -> std::collections::hash_map::Keys<<E as Estimator>::ValidatorName, HashSet<Message<E>>>
    {
        self.0.keys()
    }

    /// Get the set values, i.e. the messages.
    pub fn values(
        &self,
    ) -> std::collections::hash_map::Values<'_, <E as Estimator>::ValidatorName, HashSet<Message<E>>>
    {
        self.0.values()
    }

    /// Update the data structure by adding a new message. Return true if the new message is a
    /// valid latest message, i.e. the first message of a validator or a message that is not in the
    /// justification of the existing latest messages.
    pub fn update(&mut self, new_msg: &Message<E>) -> bool {
        let sender = new_msg.sender();
        if let Some(latest_msgs_from_sender) = self.get(sender).cloned() {
            latest_msgs_from_sender
                .iter()
                .filter(|&old_msg| new_msg != old_msg)
                .fold(false, |acc, old_msg| {
                    let new_independent_from_old = !new_msg.depends(old_msg);
                    // equivocation, old and new do not depend on each other
                    if new_independent_from_old && !old_msg.depends(new_msg) {
                        self.get_mut(sender)
                            .map(|msgs| msgs.insert(new_msg.clone()))
                            .unwrap_or(false)
                            || acc
                    }
                    // new actually older than old
                    else if new_independent_from_old {
                        acc
                    }
                    // new newer than old
                    else {
                        self.get_mut(sender)
                            .map(|msgs| msgs.remove(old_msg) && msgs.insert(new_msg.clone()))
                            .unwrap_or(false)
                            || acc
                    }
                })
        } else {
            // no message found for this validator, so new_msg is the latest
            self.insert(sender.clone(), [new_msg.clone()].iter().cloned().collect());
            true
        }
    }

    /// Checks whether the new message equivocates with latest messages.
    pub(crate) fn equivocate(&self, msg_new: &Message<E>) -> bool {
        self.get(msg_new.sender())
            .map(|latest_msgs| latest_msgs.iter().any(|m| m.equivocates(&msg_new)))
            .unwrap_or(false)
    }
}

impl<'z, E: Estimator> From<&'z Justification<E>> for LatestMsgs<E> {
    /// Extract the latest messages of each validator from a justification.
    fn from(j: &Justification<E>) -> Self {
        let mut latest_msgs: LatestMsgs<E> = LatestMsgs::empty();
        let mut queue: VecDeque<Message<E>> = j.iter().cloned().collect();
        while let Some(msg) = queue.pop_front() {
            if latest_msgs.update(&msg) {
                msg.justification()
                    .iter()
                    .for_each(|m| queue.push_back(m.clone()));
            }
        }
        latest_msgs
    }
}

/// Set of latest honest messages for each validator.
pub struct LatestMsgsHonest<E: Estimator>(HashSet<Message<E>>);

impl<E: Estimator> LatestMsgsHonest<E> {
    /// Create an empty latest honest messages set.
    fn empty() -> Self {
        LatestMsgsHonest(HashSet::new())
    }

    /// Insert message to the set.
    fn insert(&mut self, msg: Message<E>) -> bool {
        self.0.insert(msg)
    }

    /// Remove messages of a validator.
    pub fn remove(&mut self, validator: &<E as Estimator>::ValidatorName) {
        self.0.retain(|msg| msg.sender() != validator);
    }

    /// Filters the latest messages to retreive the latest honest messages and remove equivocators.
    pub fn from_latest_msgs(
        latest_msgs: &LatestMsgs<E>,
        equivocators: &HashSet<<E as Estimator>::ValidatorName>,
    ) -> Self {
        latest_msgs
            .iter()
            .filter_map(|(validator, msgs)| {
                if equivocators.contains(validator) || msgs.len() != 1 {
                    None
                } else {
                    msgs.iter().next()
                }
            })
            .fold(LatestMsgsHonest::empty(), |mut acc, msg| {
                acc.insert(msg.clone());
                acc
            })
    }

    pub fn iter(&self) -> std::collections::hash_set::Iter<Message<E>> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn mk_estimate<U: WeightUnit>(
        &self,
        validators_weights: &validator::Weights<<E as Estimator>::ValidatorName, U>,
    ) -> Result<E, E::Error> {
        E::estimate(&self, validators_weights)
    }
}
