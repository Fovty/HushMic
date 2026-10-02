# Pause protection with a fixed voice reference

Status: ready for final specification review, 2026-10-02. All eleven interview decisions are settled, including the lifecycle, controls, and test seam. Overall confirmation of shared understanding is pending. This document does not authorize implementation.

## Problem Statement

When the user speaks, HushMic's quality denoiser suppresses background voices and noise. During a longer pause, continuing background speech can become audible again; suppression returns when the user resumes. The user wants HushMic to retain a reference to their voice so a pause does not cause another speaker to become the voice that passes through.

The current denoiser enhances speech without an enrolled-speaker reference. Loudness alone cannot identify a person: the user can speak quietly or move away from the microphone, and another person or a television can become louder. The reported sequence has not yet been reproduced on a captured recording, so the precise leakage mechanism and its timing remain unmeasured.

## Solution

Add pause protection based on a fixed voice reference established through deliberate enrollment. Detect whether the enrolled speaker is present, reduce output when they are absent, and retain the existing denoiser when they speak. Keep the same enrolled speaker across pauses rather than automatically choosing a new first or loudest voice.

Favor preserving the enrolled speaker's words when classifications are uncertain, accepting some background leakage rather than frequently cutting their speech. Pause protection complements existing denoising; it does not introduce a new system for separating simultaneous speakers.

The agreed user flow is: enroll a clean voice sample, explicitly enable pause protection, and continue using the HushMic virtual microphone. Save the voice reference locally across restarts and discard the raw enrollment recording. Require a 200 ms total HushMic processing ceiling with first syllables preserved. If the detector fails, continue ordinary denoising and clearly report pause protection as unavailable.

## User Stories

These stories express the agreed requirements and their direct implications. Model-specific enrollment limits, detection thresholds, and measured performance must be established through the agreed offline evaluation.

1. As a HushMic user, I want background speech suppressed while I am silent, so that other people's conversations do not become the microphone's foreground speech during my pauses.
2. As an enrolled speaker, I want the same voice reference retained through long pauses, so that HushMic continues to recognize my return without choosing another speaker.
3. As an enrolled speaker, I want my speech to remain audible when I resume after a pause, so that listeners hear the start of my next sentence.
4. As an enrolled speaker, I want my first syllables preserved, so that speaker detection does not clip word beginnings.
5. As an enrolled speaker, I want ordinary quiet speech preserved, so that speaking softly does not make HushMic treat me as background noise.
6. As an enrolled speaker, I want loudness to remain an auxiliary cue rather than my identity, so that relative volume changes do not select another person.
7. As a HushMic user, I want to deliberately provide the enrollment sample, so that a television or another speaker is not automatically selected as me.
8. As an enrolled speaker, I want uncertain classifications to favor retaining my words, so that avoiding background leakage does not make my own speech repeatedly disappear.
9. As a HushMic user, I want existing denoising to continue while I speak, so that pause protection also retains the application's current noise-reduction benefit.
10. As a HushMic user, I want pause protection to be optional and enabled explicitly, so that I choose when to use speaker-dependent output.
11. As a HushMic user, I want ordinary suppression available before enrollment, so that setting up a voice reference is not required to use the application.
12. As a HushMic user, I want the enrollment flow to explain how to provide a clean sample, so that the resulting reference represents my voice.
13. As a HushMic user, I want unusable enrollment samples rejected with a retry explanation, so that an empty, clipped, or unsuitable sample does not silently become my reference.
14. As a HushMic user, I want canceling or failing enrollment to preserve my existing reference, so that an unsuccessful replacement does not erase a working setup.
15. As an enrolled speaker, I want one fixed voice reference saved locally across restarts, so that I do not need to enroll each time I start HushMic.
16. As an enrolled speaker, I want enrollment and live detection processed locally, so that my voice is not sent to a remote service.
17. As an enrolled speaker, I want the enrollment recording discarded after reference creation, so that retaining the reference does not also retain a recording of what I said.
18. As an enrolled speaker, I want to replace or delete my reference explicitly, so that I control which voice is enrolled and whether a reference remains stored.
19. As a HushMic user, I want requested pause protection distinguished from protection that is actually available, so that I know when ordinary denoising is operating without speaker detection.
20. As an enrolled speaker, I want detector failures to leave ordinary denoising available with a clear status, so that a failed detector does not unnecessarily silence me.
21. As a HushMic user, I want supported CPU fallback to retain pause protection while the detector remains healthy, so that changing denoising tiers does not silently drop the voice restriction.
22. As a HushMic user, I want suppression strength to respect pause protection, so that mixing original audio back into output does not bypass the pause gate.
23. As a HushMic user, I want mute to produce silence and bypass to expose original audio with protection shown as inactive, so that the existing modes retain clear meanings.
24. As a HushMic user, I want microphone changes to retain my enrolled identity while resetting stream history, so that changing devices does not automatically enroll a different person.
25. As a HushMic user, I want processing delay to stay within the declared budget and audio to remain stable, so that pause protection remains suitable for conversation.
26. As a HushMic user, I want to compare ordinary denoising with pause protection on a representative speech-and-pause recording, so that I can hear whether it reduces leakage without damaging my words.

## Implementation Decisions

### Confirmed direction and constraints

- Use the project's terms: enrolled speaker, enrollment, voice reference, pause protection, and microphone profile. A microphone profile stores denoising preferences, not speaker identity.
- Introduce enrolled-speaker presence detection alongside the existing denoiser. A presence gate controls whether the denoised mixture is emitted; it does not extract a single speaker from overlapping speech.
- Establish the voice reference through explicit enrollment. Do not choose the first or loudest voice automatically, change identity during pauses, or continuously update the reference from live audio.
- Preserve the enrolled speaker's speech as the first priority when balancing uncertain classifications against background leakage. This does not mean opening for every low-confidence frame: thresholds and continuity require evaluation against real recordings.
- Keep the existing denoising models. Their current inference contract accepts audio spectrum and recurrent state, with no speaker-reference input; freezing that state would not create a speaker identity.
- The audio stream is 48 kHz mono in 10 ms hops, with approximately 100 ms of current declared processing latency. Detector decisions must apply to the corresponding delayed audio, including after stream resets and worker stalls.
- Put model inference and any required resampling or enrollment work outside the real-time callback. Reuse the existing worker and alignment mechanisms; avoid inference, blocking, locks, or allocation in the callback.
- Apply pause protection to the final suppression mixture so the attenuation limiter cannot mix background audio back in after the gate. Actual placement must also respect output ramps, alignment, and explicit modes.
- Prefer the existing microphone-capture, model-loading, configuration, status, and control patterns. Detector choice remains unselected; no new dependency or interface is justified solely by this draft.

### Confirmed product defaults

- **Activation:** pause protection is off by default and has an explicit Enable action after successful enrollment. Enrollment itself does not enable it automatically. Turning protection off does not delete the reference.
- **Persistence:** retain one local voice reference across restarts until explicit replacement or deletion. Process enrollment locally; discard the raw sample on success, failure, and cancellation. Do not include recordings or reference contents in diagnostic output.
- **Latency:** require at most 200 ms total HushMic processing latency with pause protection enabled, preserving first syllables. This is an agreed acceptance ceiling, not a measured capability or a promise about any candidate detector. Device and calling-application delays are outside this processing measurement.
- **Failure:** if the reference cannot be loaded, detector initialization fails, or detection becomes unhealthy, continue ordinary denoising and report pause protection as unavailable. This fallback concerns the additional detector; it does not relax existing silence-on-worker-failure behavior or mute semantics.

### Lifecycle and integration requirements

- Provide a small voice-setup window reached from the tray, with Learn, Replace, and Forget actions, plus an explicit pause-protection toggle. Expose protection status and enable/disable through the existing CLI. Reuse the existing capture and UI patterns; additional headless enrollment workflows are outside this version's agreed scope.
- Explain clean-sample requirements before recording. Validate sample suitability, reject known-invalid input, and preserve the prior reference until replacement succeeds. Required duration and quality limits depend on the selected detector and must be documented rather than guessed.
- Keep the reference independent of microphone profiles. A microphone change resets temporal audio state without changing enrolled identity. If the new recording conditions make detection unusable, report that state and allow explicit enrollment replacement rather than silently relearning another voice.
- Preserve pause protection through quality/light denoising fallback when the detector is healthy. If the detector itself cannot keep up, use the chosen failure behavior and report the change. No automatic path should quietly expose original audio.
- In mute, output is silent. In bypass or explicit passthrough, output remains original audio and pause protection is shown as inactive. Returning to suppression uses the existing reference without re-enrollment.
- Deleting the reference disables active protection, clears the stored reference and any active copy, and returns to ordinary denoising. Cancellation and failed replacement retain the previous reference and explicit enable preference.
- Configuration must distinguish the user's request for protection from the current operational availability. Existing installations without a voice reference retain ordinary denoising and do not implicitly enroll anyone.
- Report active, disabled, and unavailable protection through existing status/diagnostic surfaces, with a reason for unavailability. Diagnostics must not disclose the enrolled voice data.
- Select a detector only after an offline streaming evaluation establishes preservation, leakage reduction, runtime compatibility, licensing suitability, and sustainable CPU cost. Packaging must make required model assets available without uploading speech or introducing an unexpected live-call download.

## Testing Decisions

- **Agreed primary seam:** drive the complete plugin through its existing audio callback harness, including the actual processing worker, rings, alignment, denoising, suppression-strength mixing, and final emitted samples. Add enrollment/reference provisioning to this harness rather than inventing a separate detector-only functional test API. The user confirmed this seam, the supporting configuration checks, and the final live PipeWire check in decision 11.
- Evaluate candidate detectors offline before committing to full enrollment UI work. The candidate must reduce pause leakage while preserving quiet speech and word beginnings, stay within the 200 ms total processing ceiling, and sustain real-time processing without growing delay. If no candidate meets the agreed limits, report the finding rather than relaxing them or shipping an unverified feature.
- Good tests assert externally observable audio and status behavior. Do not assert private speaker embeddings, recurrent-state contents, individual neural-layer outputs, or a specific threshold formula. A mocked detector cannot establish actual speaker rejection quality.
- Follow the existing plugin latency tests, real-model adaptive-engine tests on public audio clips, and asynchronous worker tests with scripted failures and CPU pressure. Use the production detector and a fixed enrollment sample for acoustic acceptance; reserve injected failures or scripted timing for deterministic recovery checks.
- Build one reusable scenario fixture with labeled target-presence intervals and separately available clean speaker sources: enrolled speech, a long target-free interval with continuous competing speech, then resumed enrolled speech. Include short and long pauses, quiet speech, a louder interfering voice, noise, and a competing voice that starts before the enrolled speaker.
- Use an enrollment sample separate from the evaluation speech. Validate that target-free intervals do not become a new identity and that target speech following a long pause retains its onset and words.
- Compare protection enabled and disabled with the same ordinary denoising settings, aligned for their measured delays. Measure background leakage over labeled target-free intervals, target-speech retention, onset loss, actual processing latency, and sustained CPU cost. Loudness measurements alone are insufficient evidence that the correct speaker is preserved.
- Verify the final output across representative suppression strengths, quality/light fallback, protection enabled/disabled, mute, bypass, detector failure, recovery, and stream reset. In particular, exercise strength settings that mix original audio into output.
- Reuse existing configuration round-trip and control-socket integration checks for enrollment/reference lifecycle and requested-versus-operational status. Verify failed replacement preserves the old reference, deletion removes it, raw enrollment recordings are discarded, and restarts follow the selected persistence policy.
- Reuse worker stress patterns to establish bounded output, finite samples, correct alignment, and recovery without replaying stale audio. Benchmark the full denoiser-plus-detector pipeline on a documented target machine; parameter counts or detector decision cadence are not substitutes for measurements.
- Run a final live PipeWire smoke check for microphone selection and playback routing after automated plugin-level acceptance. This supplements the existing primary seam rather than replacing it with a new UI automation suite.
- Before shipping, pin the evaluation material, reference machine, and numeric leakage, speech-retention, and CPU thresholds from a measured baseline. Neither the exact user recording nor a candidate integration has yet been measured; this draft deliberately does not invent passing quality numbers. The confirmed 200 ms total processing ceiling is a hard acceptance requirement.
- Real-model acceptance must fail clearly when required assets are absent in its designated CI job, following the repository's existing mandatory-asset convention. A skipped model test does not establish speaker-detection quality.

## Out of Scope

- A new model that separates simultaneously speaking people. The existing denoiser continues to handle overlap as it currently does.
- Automatic enrollment of the first or loudest voice, continuous reference adaptation, or silently switching to another speaker during pauses.
- Per-user fine-tuning or training a new denoising model.
- Multiple enrolled speakers, a speaker directory, automatic diarization, and cloud synchronization.
- Additional headless enrollment workflows; this version provides the voice-setup window and CLI status/enable/disable.
- Remote processing of enrollment or live microphone audio under the confirmed local-only design.
- Guaranteed rejection of every other speaker, speaker authentication, or a privacy/security boundary. Favoring speech preservation and ordinary-denoising fallback can allow background leakage.
- Changes to device or calling-application latency, and a general redesign of the denoiser, worker, tray, or configuration system.
- Implementing, installing model dependencies, committing, or deploying the feature as part of this specification request.

## Further Notes

- All eleven interview decisions are settled. Numeric acoustic and CPU criteria, model-specific sample validation limits, and detector choice are investigation outputs to establish and document during offline evaluation, not unmeasured implementation promises.
- The specification intentionally separates a desired fixed voice reference from a proven model choice. [Personalized speech-enhancement research](https://www.microsoft.com/en-us/research/publication/personalized-speech-enhancement-new-models-and-comprehensive-evaluation/) establishes the model class, not a ready HushMic integration.
- [nomo-pvad](https://github.com/tuya/nomo-pvad) is an example candidate, not a selected dependency. Its published interface uses a voice reference with 16 kHz streamed input and probabilities every 160 ms; it documents at least 10 seconds of enrollment and roughly 18.16 million parameters, plus a separate enrollment model. HushMic compatibility, CPU cost, and onset preservation remain unverified. A 160 ms decision cadence does not directly establish added audio latency.
- [SpeakerBeam-SS](https://www.isca-archive.org/interspeech_2024/sato24_interspeech.html) illustrates streaming target-speaker extraction, the broader overlapping-speaker approach intentionally excluded from the confirmed first-version scope.
- A recorded reproduction and an offline candidate evaluation must precede investment in full enrollment UI or production live integration. Enforce the confirmed speech-preservation priority and 200 ms processing ceiling, and document the reference machine and sustainable CPU behavior. Candidate choice and numeric quality thresholds must be settled before claiming the feature meets acceptance.
- No issue-tracker destination or project triage vocabulary has been provided. The checkout has both an origin fork and an upstream project, so publication must not infer the destination from a remote alone. Complete `/setup-matt-pocock-skills` before publishing the spec with the skill's required `ready-for-agent` label.
- The test-seam expectation check is satisfied by decision 11. Final shared-understanding confirmation and tracker setup are outstanding. This local spec has not been published or labeled, and implementation remains outside the current request.
