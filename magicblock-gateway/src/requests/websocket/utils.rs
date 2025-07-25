use json::Serialize;

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum SubResult {
    SubId(u64),
    Unsub(bool),
}
