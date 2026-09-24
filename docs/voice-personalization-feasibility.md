# Feasibility: personalizing HushMic's light model

Researched 2026-09-18; primary sources and local integration rechecked 2026-09-22. Scope: retain the user's voice and suppress other people, including after the user has been silent. Documentation/source review only; no model was trained, downloaded, benchmarked, or integrated.

## Finding

**Not as a built-in feature today. Fine-tuning the light model and integrating the result is technically plausible, but reliable rejection of everyone else's voice is unproven.** The smallest runtime change would keep DPDFNet's architecture and specialize its weights for one person. That could reuse HushMic's existing inference engine and custom-model configuration. A short voice recording cannot simply be attached to the current model: its interface has no speaker-reference input. This conclusion follows from the [upstream model](https://github.com/ceva-ip/DPDFNet/blob/main/model/dpdfnet_48khz_hr.py), [ONNX exporter](https://github.com/ceva-ip/DPDFNet/blob/main/onnx_model/export_dpdfnet_48khz_hr_to_onnx.py), [local runtime](../crates/hushmic-denoiser/src/model.rs), and [model configuration](../crates/hushmic/src/config_cli.rs).

The main uncertainty is learned behavior, not routing audio into the app. Even purpose-built personalized enhancement models can leak another speaker after the target is silent for a long time, or suppress the target accidentally. Microsoft specifically documents this trade-off and investigates personalized voice-activity detection during training to reduce it. A voice profile alone is therefore no guarantee. [Primary research](https://www.microsoft.com/en-us/research/publication/breaking-the-trade-off-in-personalized-speech-enhancement-with-cross-task-knowledge-distillation/).

## What the light model provides

The official profile lists `dpdfnet2_48khz_hr` at **2.58 million parameters, 2.42 G MACs, and about 10 MB ONNX**, versus 3.63 million parameters and 7.17 G MACs for the quality model. Those are publisher figures, not measurements on this machine. The upstream README documents downloadable PyTorch checkpoints and ONNX export. It also declares Apache-2.0 licensing. [DPDFNet README](https://github.com/ceva-ip/DPDFNet).

The 48 kHz implementation is a trainable PyTorch model whose forward method takes a waveform. The streaming exporter takes a checkpoint and block count, then produces `spec`/`state_in` inputs and `spec_e`/`state_out` outputs. No enrollment audio or speaker embedding is accepted. Changing weights while preserving architecture should preserve approximate inference cost; this is an engineering inference, pending an actual export and benchmark. [Model source](https://github.com/ceva-ip/DPDFNet/blob/main/model/dpdfnet_48khz_hr.py), [export source](https://github.com/ceva-ip/DPDFNet/blob/main/onnx_model/export_dpdfnet_48khz_hr_to_onnx.py).

**Missing piece:** I found model definitions, inference, and export, but no documented complete personalization trainer in the inspected upstream root and model directory. A training loop, mixture generation, objectives, and validation would need to be assembled. The paper describes generic enhancement training and long-sequence fine-tuning; that is not a demonstrated recipe for recognizing this user's voice. [Repository contents](https://github.com/ceva-ip/DPDFNet), [model directory](https://github.com/ceva-ip/DPDFNet/tree/main/model), [paper](https://arxiv.org/html/2512.16420v1). Checkpoint download availability is documented upstream, but the hosted binaries were not downloaded or load-tested in this investigation.

## Training versus enrollment

| Approach | What changes | Fit for this request |
|---|---|---|
| Fine-tune DPDFNet for one user | Model weights; identity is implicit in the specialized model | Smallest potential runtime change, but experimental rejection quality |
| Reference-conditioned extraction | A trained extractor receives an embedding or reference of the desired voice | Designed for selecting a speaker; requires a compatible model and a new integration |
| Personalized voice-activity gate | A detector decides whether the enrolled user is speaking, then controls output gain | Directly addresses background-only intervals; cannot separate overlapping voices by itself |

Weight-based specialization is a legitimate research direction: published work adapts compact enhancement models to individual users using transfer learning or self-supervised data. It does **not** establish that DPDFNet2 will reject other speakers reliably. [Sivaraman and Kim](https://arxiv.org/abs/2104.02017), [author-maintained personalization code](https://github.com/kimsunwiub/PSE_ZeroShot_KD).

Reference-based target extraction uses enrollment speech to condition separation without retraining for each user. Real-time designs exist: SpeakerBeam-SS explicitly targets causal streaming extraction and reports lower runtime cost than its baseline. This establishes feasibility of the model class, not a ready HushMic-compatible checkpoint. Its paper also warns that VoiceFilter-Lite produces ASR features rather than communication audio. [SpeakerBeam-SS paper](https://www.isca-archive.org/interspeech_2024/sato24_interspeech.pdf).

## What a useful fine-tuning experiment needs

The following is a proposed experiment, not an upstream supported procedure:

1. Start from the light checkpoint and reproduce its exported baseline before training.
2. Record clean examples of the user across separate sessions. As an initial data-collection budget, try 30–60 minutes covering normal, quiet, and animated speech. This is a practical pilot suggestion, **not a proven minimum or success guarantee**.
3. Generate paired inputs containing the user's speech plus other voices/noise, with the user's speech alone as the target. Include other-voice-only intervals whose desired output is silence, and long pauses followed by a competing speaker. Vary levels and rooms so the model cannot succeed merely by selecting the louder voice.
4. Hold out entire recording sessions and some competing speakers. Evaluate preservation of quiet words and word onsets, suppression during overlap, and leakage after long silence. Use a loss that handles silent targets explicitly; do not rely solely on a relative speech metric where the reference is zero.
5. Export and compare streaming output against the trained model before trying a live meeting.

These choices address the reported failure and known preservation/leakage trade-off. Training only on clean recordings of the user gives no explicit examples of which other speech to reject. That is a training-design inference, supported by how target-extraction research constructs target/interferer mixtures and evaluates suppression errors. [SpeakerBeam-SS training setup](https://www.isca-archive.org/interspeech_2024/sato24_interspeech.pdf), [PSE evaluation research](https://www.microsoft.com/en-us/research/publication/personalized-speech-enhancement-new-models-and-comprehensive-evaluation/).

No defensible training-time, GPU-memory, or required-recording guarantee follows from the available evidence. Those require a working pilot and the actual hardware.

## Integration into this checkout

The current runtime uses 48 kHz mono, 480-sample hops, and a 960-point transform. It passes a `[1, 1, 481, 2]` spectrum plus recurrent state to ONNX and initializes state using model metadata. A same-architecture fine-tuned export could reuse these mechanics; different tensor shapes, normalization, state layout, or delay would require more work. [Model loader](../crates/hushmic-denoiser/src/model.rs), [denoiser](../crates/hushmic-denoiser/src/denoiser.rs), [engine constants](../crates/hushmic-denoiser/src/lib.rs).

**Compatible custom-model selection already exists.** The CLI accepts any existing `<model_dir>/<id>.onnx` model ID, and the controller supports `HUSHMIC_MODEL_DIR` and passes the selected file to the plugin. A successfully fine-tuned export that preserves the current interface could therefore be loaded without adding a new inference backend or model selector. The tray only exposes stock choices; custom UI is optional for an experiment. Keep the artifact separate from packaged files: asset provisioning checks hashes, so overwriting a stock model is fragile. This is source-level compatibility, not a tested custom-model deployment. [Model validation](../crates/hushmic/src/config_cli.rs), [controller](../crates/hushmic/src/controller.rs), [tray](../crates/hushmic/src/tray.rs), [asset setup](../scripts/setup-assets.sh).

The plugin identifies light models by a `dpdfnet2` filename prefix; a different name can select a quality-to-stock-light-to-raw fallback ladder. Even a custom `dpdfnet2` model can fall back to raw audio. Some controller decisions additionally compare the exact stock model ID. A strict personalized mode must account for these paths: neither a generic model nor raw audio preserves the intended speaker restriction. Mixing original audio back in through suppression-strength settings also reintroduces interference. These are separate integration concerns from simply loading compatible weights. [Plugin](../crates/dpdfnet-ladspa/src/lib.rs), [controller](../crates/hushmic/src/controller.rs), [attenuation](../crates/hushmic-denoiser/src/attn.rs).

This checkout declares about 100 ms of engine/worker buffering combined, before device/browser delays; a new detector adds to the timing problem. Existing parity, latency, and runtime benchmarks help validate integration, but the silence-only speed benchmark cannot establish speaker rejection. [Controller latency](../crates/hushmic/src/controller.rs), [parity test](../crates/hushmic-denoiser/tests/parity.rs), [latency test](../crates/hushmic-denoiser/tests/latency.rs), [benchmark](../crates/hushmic-denoiser/examples/bench_rtf.rs).

## Alternative worth testing before training

Tuya's `nomo-pvad` supplies pretrained inference for enrolled-speaker detection: at least 10 seconds of enrollment is shown, input is 16 kHz, and decisions arrive every 160 ms. It publishes approximately 18.16 million parameters, so it is **not automatically lighter than DPDFNet**. It includes PyTorch weights, but no training code; no ready HushMic ONNX integration was established. The authors warn about similar voices and mismatched recording conditions. [Official repository](https://github.com/tuya/nomo-pvad).

An offline detector experiment could show whether “mute while this user is absent” solves the immediate problem without training. For live use, buffering to preserve the opening syllable trades latency against leakage; without buffering, detection can arrive after speech begins. Gating also leaves overlapping background voices to the denoiser. These are consequences of a chunk-level gate, not measured results for this machine.

**Recommendation:** validate the exact silence-then-background-speaker sequence offline before building enrollment UI or modifying the live pipeline. If the requirement is specifically to personalize DPDFNet2, run the bounded fine-tuning experiment first. Integrate only after held-out recordings demonstrate improvement without losing the user's words. Both routes are possible development projects; neither is a configuration switch or a guaranteed fix.

Research method: Context7 resolved `/ceva-ip/dpdfnet` and was queried again on 2026-09-22 for personalization/export documentation; the current upstream README, waveform model, streaming exporter, Microsoft research, SpeakerBeam-SS paper, and nomo-pvad repository were inspected. Local custom-model configuration and fallback behavior were rechecked. Existing uncommitted application changes were not edited.
