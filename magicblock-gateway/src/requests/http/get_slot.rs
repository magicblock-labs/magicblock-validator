use hyper::Response;
use magicblock_accounts_db::AccountsDb;

use crate::{
    requests::{payload::ResponsePayload, JsonRequest},
    utils::JsonBody,
};

pub(crate) fn handle(
    request: JsonRequest,
    accountsdb: &AccountsDb,
) -> Response<JsonBody> {
    let slot = accountsdb.slot();
    Response::new(ResponsePayload::encode_no_context(&request.id, slot))
}
