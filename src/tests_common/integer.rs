// Core CBC Casper
// Copyright (C) 2018 - 2020  Coordination Technology Ltd.
// Authors: pZ4 <pz4@protonmail.ch>,
//          Lederstrumpf,
//          h4sh3d <h4sh3d@truelevel.io>
//          roflolilolmao <q@truelevel.ch>
//
// This file is part of Core CBC Casper.
//
// Core CBC Casper is free software: you can redistribute it and/or modify it under the terms
// of the GNU Affero General Public License as published by the Free Software Foundation, either
// version 3 of the License, or (at your option) any later version.
//
// Core CBC Casper is distributed in the hope that it will be useful, but WITHOUT ANY
// WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR
// PURPOSE. See the GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License along with the Core CBC
// Rust Library. If not, see <https://www.gnu.org/licenses/>.

use std::collections::HashSet;
use std::iter::FromIterator;

use crate::estimator::Estimator;
use crate::justification::LatestMessagesHonest;
use crate::message::Message;
use crate::util::weight::{WeightUnit, Zero};
use crate::validator;

type Validator = u32;

#[derive(Clone, Eq, Debug, Ord, PartialOrd, PartialEq, Hash, serde_derive::Serialize)]
pub struct IntegerWrapper(pub u32);

impl IntegerWrapper {
    pub fn new(estimate: u32) -> Self {
        IntegerWrapper(estimate)
    }
}

#[cfg(feature = "integration_test")]
impl<V: validator::ValidatorName> From<V> for IntegerWrapper {
    fn from(_validator: V) -> Self {
        IntegerWrapper::new(u32::default())
    }
}

#[derive(Clone, Eq, Debug, Ord, PartialOrd, PartialEq, Hash)]
pub struct Tx;

#[derive(Debug, PartialEq)]
pub struct Error(&'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "{}", self.0)
    }
}

impl std::error::Error for Error {}

impl std::convert::From<&'static str> for Error {
    fn from(string: &'static str) -> Self {
        Error(string)
    }
}

/// the goal here is to find the weighted median of all the values
impl Estimator for IntegerWrapper {
    type ValidatorName = Validator;
    type Error = Error;

    fn estimate<U: WeightUnit>(
        latest_messages: &LatestMessagesHonest<Self>,
        validators_weights: &validator::Weights<Validator, U>,
    ) -> Result<Self, Self::Error> {
        let mut messages_sorted_by_estimate = Vec::from_iter(latest_messages.iter().fold(
            HashSet::new(),
            |mut latest, latest_from_validator| {
                latest.insert(latest_from_validator);
                latest
            },
        ));
        messages_sorted_by_estimate.sort_unstable_by(|a, b| a.estimate().cmp(&b.estimate()));

        // get the total weight of the validators of the messages
        // in the set
        let total_weight = messages_sorted_by_estimate
            .iter()
            .fold(<U as Zero<U>>::ZERO, |acc, weight| {
                acc + validators_weights.weight(weight.sender()).unwrap_or(U::NAN)
            });

        let mut running_weight = <U as Zero<U>>::ZERO;
        let mut message_iter = messages_sorted_by_estimate.iter();
        let mut current_message: Result<&&Message<IntegerWrapper>, &str> = Err("no message");

        // since the messages are ordered according to their estimates,
        // whichever estimate is found after iterating over half of the total weight
        // is the consensus
        while running_weight + running_weight < total_weight {
            current_message = message_iter.next().ok_or("no next message");
            running_weight += current_message
                .and_then(|message| {
                    validators_weights
                        .weight(message.sender())
                        .map_err(|_| "Can't unwrap weight")
                })
                .unwrap_or(U::NAN)
        }

        // return said estimate
        current_message
            .map(|message| message.estimate().clone())
            .map_err(From::from)
    }
}
