use super::prelude::*;

impl HttpDispatcher {
    pub(crate) fn get_version(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let version = magicblock_version::Version::default();
        // @@@ TODO(bmuddha): use real solana core version and git commit
        let version = json::json! {{
            "solana-core": "",
            "feature-set": version.feature_set,
            "git-commit": "",
            "magicblock-core": version.to_string(),
        }};
        Ok(ResponsePayload::encode_no_context(&request.id, version))
    }
}
