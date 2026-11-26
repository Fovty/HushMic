//! The speakers chain's PipeWire conf: a selectable virtual output
//! `hushmic_sink` ("HushMic Speakers") whose sound the plugin cleans and
//! `hushmic_playback` plays on a real output.

use super::{LINK_GROUP, PLAYBACK, SINK};
use crate::controller::{spa_escape, Paths, CONF_BASE, LATENCY_SAMPLES, PINNED_QUANTUM};

/// What one speakers conf carries.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfSpec<'a> {
    /// The real output `hushmic_playback` is pinned to; None = it follows
    /// the system default.
    pub target: Option<&'a str>,
    /// The strength (dB cap) it starts with.
    pub attn: f32,
    /// PipeWire < 0.3.64: also the legacy `node.target`.
    pub legacy_node_target: bool,
    /// PipeWire >= 1.6: the report-only latency node.
    pub report_latency: bool,
}

/// Render a self-contained `pipewire -c` conf for the speakers chain. Pure.
///
/// Mono only: the sink takes one channel and PipeWire downmixes what apps
/// play into it (voice is the same on both channels), and the playback
/// stream's one channel goes to both speakers. The sink is selectable
/// only: nothing here makes it the default. The chain starts in Suppress;
/// a momentary bypass ("hold to hear the original") goes through the
/// control file.
pub fn render(spec: &ConfSpec, paths: &Paths) -> String {
    let target = match spec.target {
        Some(name) => {
            let esc = spa_escape(name);
            let mut t = format!("        target.object  = \"{esc}\"\n");
            if spec.legacy_node_target {
                t.push_str(&format!("        node.target     = \"{esc}\"\n"));
            }
            t
        }
        None => String::new(),
    };
    let attn = if spec.attn.is_finite() {
        spec.attn.clamp(0.0, 100.0)
    } else {
        100.0
    };
    let (latency_node, links) = if spec.report_latency {
        let secs = LATENCY_SAMPLES as f64 / 48_000.0;
        (
            format!(
                "\n          {{ type   = builtin\n            name   = hushmic_speakers_latency\n            label  = delay\n            config = {{ \"max-delay\" = 0.001 \"latency\" = {secs} }}\n            control = {{ \"Delay (s)\" = 0.0 }}\n          }}"
            ),
            "\n        links = [\n          { output = \"hushmic_speakers_dsp:Output\" input = \"hushmic_speakers_latency:In\" }\n        ]"
                .to_string(),
        )
    } else {
        (String::new(), String::new())
    };
    format!(
        r#"# hushmic speakers filter-chain host (generated; do not edit).
# The selectable output "HushMic Speakers": what apps play into it is
# cleaned and played on a real output.
context.properties = {{
    log.level = 2
}}
{base}
  {{ name = libpipewire-module-filter-chain
    flags = [ nofail ]
    args = {{
      node.description = "HushMic Speakers"
      media.name       = "HushMic Speakers"
      filter.graph = {{
        nodes = [
          {{ type   = ladspa
            name   = hushmic_speakers_dsp
            plugin = "{plugin}"
            label  = "dpdfnet_mono"
            control = {{ "Attenuation Limit (dB)" = {attn} "Mode" = 0 }}
          }}{latency_node}
        ]{links}
      }}
      capture.props = {{
        # The output apps pick: never made the default by HushMic (the
        # model is speech-trained and adds about 80 ms). Its volume is the
        # user's and is restored like any output's.
        node.name          = "{sink}"
        node.description   = "HushMic Speakers"
        media.class        = Audio/Sink
        node.link-group    = "{group}"
        audio.rate         = 48000
        audio.channels     = 1
        audio.position     = [ MONO ]
        # Pin the graph quantum while something plays (issue #10): the
        # async output margin and the declared latency are sized for it.
        node.force-quantum = {pin}
      }}
      playback.props = {{
        # The stream toward the real output. Passive: with nothing playing
        # into the sink the chain idles. Without a target it follows the
        # system default output. Its own link group keeps the session
        # manager from looping it into the sink.
        node.name           = "{playback}"
        node.description    = "HushMic Speakers output"
        media.name          = "HushMic Speakers output"
        node.passive        = true
        node.link-group     = "{group}"
        # A mixer's volume or mute on this internal stream is not restored,
        # nor a target HushMic gave it (the output is HushMic's setting).
        state.restore-props = false
        state.restore-target = false
        audio.rate     = 48000
        audio.channels = 1
        audio.position = [ MONO ]
{target}      }}
    }}
  }}
]
"#,
        base = CONF_BASE,
        plugin = spa_escape(&paths.plugin_so.display().to_string()),
        attn = attn,
        latency_node = latency_node,
        links = links,
        sink = SINK,
        playback = PLAYBACK,
        group = LINK_GROUP,
        pin = PINNED_QUANTUM,
        target = target,
    )
}
