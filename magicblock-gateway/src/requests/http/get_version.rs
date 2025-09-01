use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_version(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let version = magicblock_version::Version::default();

        let version = json::json! {{
            "solana-core": &version.solana_core,
            "feature-set": version.feature_set,
            "git-commit": &version.git_version,
            "magicblock-core": version.to_string(),
        }};
        Ok(ResponsePayload::encode_no_context(&request.id, version))
    }
}
