/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use openssl::hash::Hasher;
use serde_derive::{Deserialize, Serialize};
use std::fmt;

#[derive(Serialize, Deserialize, Clone, Debug, Hash, Ord, PartialOrd, PartialEq, Eq)]
pub(crate) struct SHA512String(pub(crate) String);

impl fmt::Display for SHA512String {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl SHA512String {
    pub(crate) fn from_hasher(hasher: &mut Hasher) -> Self {
        Self(format!(
            "sha512:{}",
            hex::encode(hasher.finish().expect("completing hash"))
        ))
    }
}
