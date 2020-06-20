use serde_derive::{Deserialize, Serialize};
use std::fmt;

#[derive(Serialize, Deserialize, Clone, Debug, Hash, Ord, PartialOrd, PartialEq, Eq)]
pub(crate) struct SHA512String(pub(crate) String);

impl fmt::Display for SHA512String {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
