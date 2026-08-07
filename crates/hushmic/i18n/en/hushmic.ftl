### English catalog — the complete fallback every other language falls
### back to per message. Keys are grouped by surface: tray- (tray menu and
### title), ab- (the A/B test window), about- (the About window), notify-
### (desktop notifications). Diagnostics, logs and CLI output are not
### localized on purpose: bug reports must be readable upstream.


## Tray icon title and menu

# "HushMic" is the product name — keep it as-is in every language (only
# the parenthesized state words translate). Numbers throughout the catalog
# ($secs, $db) arrive preformatted with a period decimal separator; the
# format is fixed, translate the words around them.
tray-title = HushMic
tray-title-bypass = HushMic (bypass)
tray-title-muted = HushMic (muted)
tray-title-error = HushMic (error)

tray-test-mic = Test my mic…
tray-test-running = Mic test running…

tray-mode = Mode
tray-mode-suppress = Noise suppression
tray-mode-bypass = Bypass
tray-mode-mute = Mute
tray-mode-off = Off

tray-microphone = Microphone
tray-system-default = System default
# Both are appended after the device name of a configured microphone that
# is currently absent, e.g. "alsa_input.rode (unavailable)".
tray-mic-unplugged = (unplugged — using system default)
tray-mic-unavailable = (unavailable)

tray-model = Model
tray-model-dpdfnet8 = High quality (dpdfnet8)
tray-model-dpdfnet2 = Light / low-CPU (dpdfnet2)

tray-strength = Suppression strength
tray-attn-maximum = Maximum
tray-attn-strong = Strong (24 dB)
tray-attn-medium = Medium (12 dB)
tray-attn-light = Light (6 dB)

tray-set-default = Set as default microphone
tray-shortcuts-setup = Set up shortcuts…
tray-shortcuts-change = Change shortcuts…
tray-autostart = Start on login
tray-about = About HushMic…
tray-quit = Quit


## A/B test window

ab-window-title = HushMic — Test Microphone
ab-heading = Live A/B Mic Test
ab-subheading = Compare raw microphone input with HushMic filtered output

ab-channel-raw = Raw microphone
ab-channel-raw-desc = Unprocessed input straight from your device
ab-channel-filtered = Filtered by HushMic
ab-channel-filtered-desc = What other applications receive
ab-playing-badge = PLAYING

ab-status-listening = Listening
ab-status-recording = Recording
ab-status-playback = Playback
ab-status-stopped = Stopped
ab-status-no-input = No input
# Shared between the status pill and the timeline label.
ab-sample-ready = Sample ready
ab-not-monitoring = Not monitoring

ab-timeline-live = Live monitoring
# $secs — seconds remaining, preformatted with one decimal (e.g. "4.0").
ab-timeline-recording = Recording sample — { $secs } s left
ab-timeline-playing-raw = Playing raw sample
ab-timeline-playing-filtered = Playing filtered sample
ab-timeline-input-unavailable = Input unavailable

ab-record-10s = Record 10 s sample
ab-record-new = Record new sample
ab-play-raw = Play raw
ab-play-filtered = Play filtered
ab-go-live = Go live
ab-cancel = Cancel
ab-stop = Stop
ab-retry-detection = Retry detection

# The stopped-hint sentence is composed as prefix + the ab-go-live button
# label + suffix; the string literals carry the surrounding spaces.
ab-press-prefix = Press{" "}
ab-press-suffix = {" "}to resume the split view. Nothing is stored until you record a sample.

ab-summary-hint = Record a 10 s sample to measure background reduction and voice retention
ab-card-background = BACKGROUND NOISE
ab-card-voice = VOICE
# $db — a preformatted level, e.g. "−42.3 dBFS".
ab-raw-db = Raw { $db }
ab-filtered-db = Filtered { $db }
ab-silent = silent
# Reduction shown as a lower bound when the filtered floor sits below the
# measurement floor; $db is preformatted, e.g. "34.0 dB".
ab-reduction-at-least = ≥ { $db }
ab-quieter = quieter
ab-preserved = Preserved
ab-voice-unmeasurable = add clear speech to measure

# Toasts shown inside the A/B window. Toasts that relay a raw captured
# error stay English (diagnostic content); these fixed hints translate.
ab-toast-silent-mic = No audio is arriving from the microphone — if it's a wireless headset, make sure it's switched on.
# "Go live" names the button — translate it identically to ab-go-live.
ab-toast-processed-died = The processed stream ended unexpectedly — HushMic may be restarting it; try Go live in a moment.
ab-toast-abort-stream = recording aborted — the capture stream ended unexpectedly
ab-toast-abort-mic-changed = recording aborted — the microphone changed

ab-settle-title = Connecting to your microphone…
ab-settle-body = HushMic is bringing up its audio chain. This takes a few seconds.
ab-no-input-title = No microphone input
ab-no-input-body = HushMic isn't receiving audio from a microphone. Pick your mic from the tray's Microphone menu (and check it's connected and PipeWire is running), then retry.


## About window

about-window-title = About HushMic
about-version = Version { $version }
about-tagline = Real-time noise suppression for Linux
about-model = Noise model: DPDFNet
about-copy-diagnostics = Copy diagnostics
about-collecting = Collecting…
about-copied = Copied


## Desktop notifications
## Bodies that relay a captured error message stay English (diagnostic
## content); only fixed titles and bodies are localized.

notify-mictest-title = Mic test
notify-mictest-failed-title = Mic test failed
notify-mictest-recording-body = Recording { $secs } seconds — speak into your microphone…
notify-mictest-playing-raw-body = Playing the raw recording (without HushMic)…
notify-mictest-playing-clean-body = Playing the cleaned recording (what your apps hear)…
notify-mictest-finished-body = Mic test finished — the second playback should have had much less background noise.
notify-mictest-cancelled-body = Stopped — the audio settings changed during the test.
notify-mictest-blocked-running = A mic test is already running.
notify-mictest-blocked-disabled = Turn on noise suppression first — the test compares the raw microphone with the cleaned HushMic output.
notify-mictest-blocked-no-node = The HushMic virtual microphone is not up yet — try again in a few seconds.
notify-mictest-blocked-no-probe = Could not query PipeWire for the microphones — is PipeWire running?
notify-no-feeder-body = Could not find the microphone feeding HushMic.
notify-old-pipewire-body = The live A/B view needs a newer PipeWire on this system.
notify-window-open-body = The A/B test window is already open.
notify-window-fallback-body = The test window could not start — running the audio-only mic test instead.

notify-tray-failed-title = HushMic could not start
# $error — the tray host's error message, passed through untranslated.
notify-tray-failed-body = Could not register a system tray icon ({ $error }). On GNOME, install the 'AppIndicator and KStatusNotifierItem Support' extension; KDE and most other desktops provide it out of the box.

notify-chain-failed-summary = HushMic could not start the virtual microphone
notify-chain-stuck-summary = HushMic keeps losing the virtual microphone
notify-chain-stuck-body = Re-creating it keeps failing — is PipeWire running? Run `hushmic --tray` from a terminal for details.
notify-chain-flapping-summary = HushMic keeps restarting the virtual microphone
notify-chain-flapping-body = It went down and was re-created several times in the last few minutes — the audio setup may be unstable. Run `hushmic --tray` from a terminal for details.
notify-running-again-summary = HushMic is running again
notify-running-again-body = The virtual microphone is back up.

notify-reroute-summary = Another app is re-routing HushMic's microphone
notify-reroute-body = A running audio tool (EasyEffects?) keeps redirecting HushMic's input to itself. In EasyEffects, disable "Process All Input Streams" (or exclude hushmic_input), then restart HushMic.

notify-mic-fallback-body = Your microphone was disconnected — HushMic is following the system default for now.
notify-mic-return-body = Your microphone is back — HushMic switched back to it.

notify-shortcuts-title = HushMic shortcuts
notify-shortcuts-body = This desktop cannot open the shortcut editor from here. Look for HushMic in your system's keyboard shortcut settings to change the keys.

notify-autostart-denied-title = Autostart was not allowed
notify-autostart-denied-body = The desktop denied HushMic's autostart request. You can allow it in your system settings under application permissions / background apps.
