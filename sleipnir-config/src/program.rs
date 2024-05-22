/*
[[program]]
id = "wormH7q6y9EBUUL6EyptYhryxs6HoJg8sPK3LMfoNf4"
path = "Volumes/d/dev/mb/demos/magic-worm/target/deploy/program_solana.so"
*/

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProgramConfig {
    pub id: String,
    pub path: String,
}
