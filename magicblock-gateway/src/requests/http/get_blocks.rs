use hyper::Response;
use magicblock_accounts_db::AccountsDb;

use crate::{
    error::RpcError,
    requests::{payload::ResponsePayload, JsonRequest},
    unwrap,
    utils::JsonBody,
    Slot,
};

pub(crate) fn handle(
    request: JsonRequest,
    accountsdb: &AccountsDb,
) -> Response<JsonBody> {
    let params = request
        .params
        .ok_or_else(|| RpcError::invalid_request("missing params"));
    unwrap!(mut params, request.id);
    let (start, end) = parse_params!(params, Slot, Slot);
    let start =
        start.ok_or_else(|| RpcError::invalid_params("missing start slot"));
    unwrap!(start, request.id);
    let slot = accountsdb.slot();
    let end = end.map(|end| end.min(slot)).unwrap_or(slot);
    let check = (start < end).then_some(()).ok_or_else(|| {
        RpcError::invalid_params("start slot is greater than the end slot")
    });
    unwrap!(check, request.id);
    let range = (start..=end).collect::<Vec<Slot>>();
    Response::new(ResponsePayload::encode_no_context(&request.id, range))
}
