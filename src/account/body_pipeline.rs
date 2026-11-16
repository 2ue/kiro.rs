use super::{model_pipeline, *};
use crate::anthropic::body_patch::BodyPatchSet;

pub(super) struct PreparedExternalRequest {
    pub(super) body: Bytes,
    pub(super) outbound_model: Option<String>,
}

#[allow(clippy::result_large_err)]
pub(super) fn prepare_request(
    route: &AccountRouteRequest,
    pool: &Account,
) -> Result<PreparedExternalRequest, AccountError> {
    let probe = route
        .effective_raw_probe
        .as_deref()
        .filter(|probe| probe.matches_body(&route.effective_raw_body));
    let fallback_probe = probe_raw_messages_body(&route.effective_raw_body);
    let probe = probe.or(Some(&fallback_probe));
    let requested_model = probe
        .and_then(|probe| probe.model.as_deref())
        .or(route.model_hint.as_deref());
    let outbound_model = model_pipeline::outbound_model_for_raw(route, pool, requested_model)?;
    let mut patches = BodyPatchSet::default();
    if let Some(model) = outbound_model
        .as_deref()
        .filter(|model| Some(*model) != requested_model)
    {
        patches.set("/model", serde_json::Value::from(model), "model_mapping");
    }
    Ok(PreparedExternalRequest {
        body: patches.apply(&route.effective_raw_body),
        outbound_model,
    })
}
