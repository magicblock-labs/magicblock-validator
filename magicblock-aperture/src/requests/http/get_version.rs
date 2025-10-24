use super::prelude::*;

impl HttpDispatcher {
    /// Handles the `getVersion` RPC request.
    ///
    /// Returns a JSON object containing the version information of the running
    /// validator node, including the Solana core version, feature set, and
    /// git commit hash.
    pub(crate) fn get_version(
        &self,
        request: &mut JsonRequest,
    ) -> HandlerResult {
        let version = magicblock_version::Version::default();

        let version_info = json::json! {{
            "solana-core": &version.solana_core,
            "feature-set": version.feature_set,
            "git-commit": &version.git_version,
            "magicblock-core": version.to_string(),
        }};
        Ok(ResponsePayload::encode_no_context(
            &request.id,
            version_info,
        ))
    }
}
