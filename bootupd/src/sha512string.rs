/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use openssl::hash::Hasher;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Serialize, Deserialize, Clone, Debug, Hash, Ord, PartialOrd, PartialEq, Eq)]
pub(crate) struct SHA512String(pub(crate) String);

impl fmt::Display for SHA512String {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl SHA512String {
    #[allow(dead_code)]
    pub(crate) fn from_hasher(hasher: &mut Hasher) -> Self {
        Self(format!(
            "sha512:{}",
            hex::encode(hasher.finish().expect("completing hash"))
        ))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::Result;

    #[test]
    fn test_empty() -> Result<()> {
        let mut h = Hasher::new(openssl::hash::MessageDigest::sha512())?;
        let s = SHA512String::from_hasher(&mut h);
        assert_eq!("sha512:cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e", format!("{}", s));
        Ok(())
    }
}
