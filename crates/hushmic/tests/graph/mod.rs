//! Stand-in graph pieces for tests that run the daemon against fake
//! PipeWire tools: the chain and the source host as pw-dump shows them,
//! and a `pw-link` that lists their ports and the link between them.
#![allow(dead_code)]

/// The four nodes (the chain's capture in `input_state`) and the passive
/// link from the chain's output into the host, as pw-dump objects (comma
/// separated, to put inside a dump's array).
pub fn chain_objects(input_state: &str) -> String {
    let node = |id: u32, name: &str, class: &str, state: &str| {
        format!(
            r#"{{ "id": {id}, "type": "PipeWire:Interface:Node", "info": {{ "state": "{state}",
            "props": {{ "media.class": "{class}", "node.name": "{name}", "node.description": "HushMic" }} }} }}"#
        )
    };
    [
        node(70, "hushmic_input", "Stream/Input/Audio", input_state),
        node(71, "hushmic_dsp_out", "Stream/Output/Audio", input_state),
        node(72, "hushmic_feed", "Stream/Input/Audio", input_state),
        node(73, "hushmic_source", "Audio/Source", input_state),
        r#"{ "id": 80, "type": "PipeWire:Interface:Link", "info": { "output-node-id": 71,
            "input-node-id": 72, "props": { "link.passive": true } } }"#
            .to_string(),
    ]
    .join(", ")
}

/// A `pw-link` that lists the chain's output and the host's feed and the
/// link between them, and accepts links and unlinks. With `gate`, nothing
/// is listed until that file exists.
pub fn pw_link(gate: Option<&std::path::Path>) -> String {
    let wait = gate
        .map(|g| format!("[ -f {} ] || exit 0\n", g.display()))
        .unwrap_or_default();
    format!(
        r#"{wait}case "$1" in
  -o) echo "  43 hushmic_dsp_out:output_MONO" ;;
  -i) echo "  44 hushmic_feed:input_MONO" ;;
  -l) printf '  43 hushmic_dsp_out:output_MONO\n  48   |->   44 hushmic_feed:input_MONO\n' ;;
esac
exit 0"#
    )
}

/// The first line of a stand-in `pipewire`: the source host just idles.
pub const HOST_IDLES: &str = r#"case "$2" in *source.conf) exec sleep 120 ;; esac"#;
