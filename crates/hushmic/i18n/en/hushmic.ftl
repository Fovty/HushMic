### English catalog — the complete fallback every other language falls
### back to per message. Keys are grouped by surface: tray- (tray menu and
### title), rail-, live-, mic-, sc-, set- and fr- (the window: its side bar,
### Live and its last 10 seconds, Shortcuts, Settings and First run), notify- (desktop
### notifications). Diagnostics, logs and CLI output are not localized on
### purpose: bug reports must be readable upstream.

## Tray icon title and menu

# "HushMic" is the product name — keep it as-is in every language (only
# the parenthesized state words translate). Numbers throughout the catalog
# ($secs, $db) arrive preformatted with a period decimal separator; the
# format is fixed, translate the words around them.
tray-title = HushMic
tray-title-bypass = HushMic (noise suppression off)
tray-title-muted = HushMic (muted)
tray-title-attention = HushMic (needs attention)
tray-title-light = HushMic (light model)
tray-title-unfiltered = HushMic (noise suppression paused)

# The status line on top of the menu, shown only when something is unusual.
tray-status-attention = Needs attention
tray-status-reconnecting = Reconnecting…
# $device is the microphone's display name, e.g. "RODE NT-USB unplugged".
tray-status-unplugged = { $device } unplugged
# Another microphone ($standin) stands in, so calls stay clean.
tray-status-standin = { $device } unplugged · using { $standin }
# An app uses HushMic and the microphone ($mic, its name) sends no sound.
tray-status-silent = No sound from { $mic }
# The tray icon's tooltip: "HushMic: on · Zoom · Jabra headset" ($state is
# one of the words below; the apps and the microphone follow, joined by " · ").
tray-tip = HushMic: { $state }
tray-tip-on = on
tray-tip-off = off
tray-tip-muted = muted
tray-tip-bypass = noise suppression off
tray-tip-unfiltered = noise suppression paused for now
tray-tip-attention = needs attention
# The engine fell back after CPU pressure: to the light model, or to no
# model at all (the microphone still works, without noise suppression).
tray-status-light = Using Light for now
tray-status-unfiltered = Noise suppression paused for now
# Files on the Files page being cleaned (or waiting their turn).
tray-status-cleaning = { $count ->
    [one] Cleaning 1 file…
   *[other] Cleaning { $count } files…
}

tray-open = Open HushMic

tray-mode-suppress = Noise suppression
tray-mode-bypass = Noise suppression off
tray-mode-mute = Mute
tray-mode-off = Turn off HushMic

tray-microphone = Microphone
tray-system-default = System default
# What "System default" stands for right now, e.g.
# "System default (Built-in Audio)".
tray-system-default-named = System default ({ $device })
# The chosen microphone while it is not plugged in, e.g.
# "RODE NT-USB · unplugged".
tray-mic-absent = { $device } · unplugged

tray-strength-menu = Strength
tray-strength-gentle = Gentle
tray-strength-medium = Medium
tray-strength-strong = Strong
tray-strength-maximum = Maximum

tray-quit-hushmic = Quit HushMic

## The window: the rail and the pages (Live, Files, Shortcuts, Settings
## with About, First run). Every heading and caption wraps greedily, so
## keep sentences plain; the Live header's second lines must fit one line
## at the window's minimum width (about 40 English characters). "HushMic" is the product
## name: keep it as-is.

# The rail: the page links, the state words for the Mark and the More menu.
# The page list's name for screen readers.
rail-sections = Sections
rail-live = Live
# The page for HushMic Speakers, the output that cleans what apps play.
rail-speakers = Speakers
rail-files = Files
rail-settings = Settings
rail-more = More
rail-turn-off = Turn off HushMic
rail-quit = Quit HushMic
# The line under Turn off HushMic in the More menu: how it differs from
# Quit HushMic. -tray while HushMic has a tray icon, -running without.
rail-turn-off-about-tray = Keeps HushMic in the tray
rail-turn-off-about-running = Keeps HushMic running
# The More menu's keyboard shortcut, at the right of Quit HushMic: the
# keys as this desktop names them.
rail-quit-key = Ctrl+Q
# Quit asks first while an app is using HushMic or files clean (Ctrl+Q,
# More › Quit HushMic, the tray's Quit): one sheet over the page. Its body
# starts with who uses HushMic (fr-apps-using, live-sub-apps-two, …).
quit-ask-title = Quit HushMic?
# $count is the number of apps using HushMic.
quit-ask-body = { $count ->
    [one] Quitting removes the HushMic microphone from it. Your call will hear your microphone directly, with the noise.
   *[other] Quitting removes the HushMic microphone from them. They will hear your microphone directly, with the noise.
}
# More › Turn off HushMic, or Off in the tray, while an app is using
# HushMic: the same sheet. Its body starts with who uses HushMic.
off-ask-title = Turn off HushMic?
# $count is the number of apps using HushMic.
off-ask-body = { $count ->
    [one] Turning it off removes the HushMic microphone from it. Your call will hear your microphone directly, with the noise.
   *[other] Turning it off removes the HushMic microphone from them. They will hear your microphone directly, with the noise.
}
off-ask-turn-off = Turn off
# The last line of the Quit and Turn off questions: how the two differ.
# -tray while HushMic has a tray icon, -running without.
quit-ask-means = Quit closes HushMic completely.
off-ask-means-tray = Turn off keeps HushMic in the tray, so you can turn it back on.
off-ask-means-running = Turn off keeps HushMic running, so you can turn it back on.
# $count is the number of files Quit would stop; files-sub-quit follows.
quit-ask-files = { $count ->
    [one] 1 file is still cleaning.
   *[other] { $count } files are still cleaning.
}
quit-ask-quit = Quit
quit-ask-cancel = Cancel
# The state in one short word: the rail's Mark names it in its tooltip and
# for screen readers (rail-mark-name below).
pill-on = On
pill-bypass = Noise suppression off
pill-muted = Muted
pill-off = Off
pill-starting = Starting
pill-reconnecting = Reconnecting
pill-unfiltered = Paused
pill-stopped = Stopped
# The rail's Mark: tooltip and screen-reader name; $state is one of the
# words above.
rail-mark-name = HushMic: { $state }
mark-needs-attention = HushMic needs attention
# Launch: shown under the Mark while HushMic starts with the window open.
launch-starting = Starting…

# Live: the header says HushMic's state alone. The live-sub-* lines are
# the lines of the notice under the controls, what stands in an empty
# lane of the picture, the picture's name and Quit's question.
live-title-on = Removing background noise.
live-title-ready = Ready.
# No app records HushMic (Live still shows the microphone).
live-sub-no-app = No app is using HushMic right now.
# Who records HushMic (one app: fr-apps-using). $app1 and $app2 are app
# names.
live-sub-apps-two = { $app1 } and { $app2 } are using HushMic.
# $count is a number, 3 or more.
live-sub-apps-many = { $count } apps are using HushMic.
# $mic is the microphone's name, e.g. "RODE NT-USB".
live-title-silent = No sound from { $mic }.
# The same title when the microphone’s name doesn’t fit on its line (the notice names it).
live-title-silent-plain = No sound from your microphone.
# The title while the chosen microphone ($mic) is unplugged and HushMic
# uses $device meanwhile.
live-title-fallback = Using { $device }. { $mic } is unplugged.
# The same title when the microphone’s name doesn’t fit on its line (the notice names it).
live-title-fallback-plain = Using another microphone.
# Live's notice about a silent microphone, with the link under it
# (Choose a microphone).
live-sub-silent = Check that it’s on.
live-title-bypass = Noise suppression is off.
live-sub-bypass = Apps hear your raw microphone.
live-title-muted = Muted.
live-sub-muted = Apps using HushMic hear silence.
# Muted while an app records the microphone itself, not HushMic's: $apps
# names them (one app's name, notify-apps-two or notify-apps-many), $count
# is how many.
live-sub-muted-direct = { $count ->
    [one] { $apps } uses your microphone directly and still hears you.
   *[other] { $apps } use your microphone directly and still hear you.
}
live-title-unfiltered = Noise suppression paused for now.
live-title-starting = Starting.
live-sub-starting = Connecting to your microphone…
live-title-reconnecting = Reconnecting.
live-sub-reconnecting = Setting up the microphone again…
# In the microphone's lane where HushMic can't listen along (a PipeWire
# too old for it): there is no picture of the sound.
live-sub-no-picture = No picture with this PipeWire version. Noise suppression works as usual.
live-title-off = HushMic is off.
# HushMic can't work (PipeWire unreachable, its microphone down).
live-title-stopped = Noise suppression is stopped.
# The tooltip of the header's controls, disabled while HushMic is stopped.
live-stopped-tip = HushMic is stopped.
# For 4 seconds after HushMic got its microphone back after a failure.
live-title-running-again = Running again.
# Live's notice while the HushMic microphone keeps restarting.
live-flapping-lead = HushMic keeps restarting its microphone.
live-flapping-body = Your audio setup may be unstable.
live-sub-off = Apps can use HushMic once it’s on.
live-mute = Mute
# The Mute button while pressed: the microphone is muted now.
live-muted = Muted
# The lower lane's caption while muted, after "What apps hear".
live-apps-muted = Nothing, muted
# After the app's name in the lower lane's caption while apps hear the
# microphone as it is (noise suppression off, or stepped down to nothing).
live-apps-unfiltered = { $apps } · unfiltered
live-apps-unfiltered-none = Unfiltered
live-unmute = Unmute
live-switch = Noise suppression
live-turn-on = Turn on HushMic
# First run's picture name for screen readers, per state.
live-pic-on = HushMic is removing background noise for your apps.
live-pic-ready = HushMic is ready. No app is using it.
live-pic-bypass = Noise suppression is off: apps hear your raw microphone.
live-pic-unfiltered = Noise suppression paused for now: apps hear your microphone as it is.
live-pic-muted = Muted: apps using HushMic hear silence.
# Live's picture name for screen readers, per state; the header's line
# about the apps follows it.
live-pic-live = Your microphone through HushMic, live.
live-pic-live-bypass = Noise suppression is off: your raw microphone, live.
live-pic-live-unfiltered = Noise suppression paused for now: your microphone as it is, live.
live-pic-live-muted = Muted: your microphone, live; apps hear silence.
live-pic-starting = HushMic is starting.
live-pic-off = HushMic is off.
# The Live picture's description for screen readers: the microphone named
# beside its upper lane. { $mic } is the microphone's name.
live-pic-mic = Microphone: { $mic }
# Beside the lower lane's label: the apps' names, or these.
live-app-none = No app right now
live-app-anon = An app
# That place holds two short names, or the
# first name and how many more ("Zoom +3").
live-app-two = { $app1 }, { $app2 }
live-app-more = { $app } +{ $count }
# Every app using HushMic: the label's tooltip, and the picture's name
# for screen readers from three apps on. $apps is the names with commas.
live-apps-all = Using HushMic: { $apps }
# The picture's two lanes: the microphone (live-microphone) above, and
# under it what HushMic passes on.
live-apps-hear = What apps hear
# On the lane a replay plays right now.
live-lane-playing = Playing

# Speakers: its Status card (the eyebrow is set in capitals by the
# window), and its quiet line when nothing needs attention.
live-status = Status
live-status-ok = Nothing needs your attention.
# PipeWire can't be reached: the lead runs into the body in one sentence.
live-status-pipewire-lead = HushMic can’t reach PipeWire,
live-status-pipewire-body = the sound system it works with.
# HushMic's microphone is down: Live's notice says this short, with Try again and
# Copy diagnostics under it (first run's block says live-down-body).
live-status-down-body = HushMic keeps trying.
live-microphone = Microphone
live-system-default = System default
# The second and later of two devices or apps with one name.
name-repeat = { $name } ({ $n })
# $device: the device the system default is now, e.g. "Built-in Audio".
live-system-default-now = Now: { $device }
live-mic-unplugged-note = Unplugged · using { $device }
# A microphone in the list that is not plugged in.
live-mic-unplugged = Unplugged
# The microphone list's tooltip on Live.
live-mic-remembered = Strength is remembered for each microphone.
live-mic-unplugged-title = { $mic } is unplugged.
live-mic-unplugged-body = Using { $device } until it’s back.
live-mic-absent-title = { $mic } is unplugged.
# The same title when the microphone’s name doesn’t fit on its line (the notice names it).
live-mic-absent-title-plain = Your microphone is unplugged.
live-mic-absent-body = Plug it back in, or choose another microphone.
# A link under a notice.
live-mic-choose = Choose a microphone
# HushMic restarted its microphone three times, and PipeWire still
# connects nothing to it. $mic is the microphone’s name; the notice has
# the link Choose a microphone under these words.
live-mic-stuck-title = { $mic } isn’t connecting.
# The same title when the microphone’s name doesn’t fit on its line (the notice names it).
live-mic-stuck-title-plain = Your microphone isn’t connecting.
live-mic-stuck-body = Unplug it and plug it back in.
live-mic-rerouted-title = Another app keeps taking this microphone.
# The same title where it doesn’t fit on its line.
live-mic-rerouted-title-plain = Another app takes your microphone.
live-mic-rerouted-body = In EasyEffects, turn off Process All Input Streams, or exclude hushmic_input from it.
live-strength = Strength
# The first step of Live's noise suppression control, before the four
# strengths: apps hear the raw microphone.
live-strength-off = Off
strength-gentle = Gentle
strength-medium = Medium
strength-strong = Strong
strength-maximum = Maximum
strength-custom = Custom
# What each strength does: its tooltip on Live.
live-strength-gentle = Takes the edge off. Most of the room sound stays.
live-strength-medium = Clearly quieter. Some room sound stays.
live-strength-strong = Most of the noise is gone.
# Maximum, on Live, Speakers and Files alike: the pauses between words go
# silent (the room tone goes too), which a listener notices.
live-strength-maximum = Removes as much noise as it can. The pauses between words become silent.
live-strength-custom = Custom strength from the command line. Pick a step to replace it.
# Added to a strength's tooltip while noise suppression (or HushMic) is off.
live-strength-applies = Applies when noise suppression is on.
# Read out by screen readers when the strength changed without a pick in
# the window (another microphone's settings took over, the tray, the
# command line). $strength is the step's name (Maximum), $mic the
# microphone the settings belong to, $caption what the step does.
live-strength-told-for = Strength { $strength }, the setting for { $mic }. { $caption }
live-strength-told = Strength { $strength }. { $caption }
model-quality = Quality
model-light = Light
live-light-now-title = Using Light for now.
live-light-now-body = Your computer is busy. HushMic goes back to Quality by itself.
live-busy-title = Your computer is too busy for noise suppression.
live-busy-body = HushMic picks up again by itself.
live-always-light = Always use Light

# Live: the page when HushMic cannot work at all.
live-pipewire-title = HushMic can’t reach PipeWire.
live-pipewire-body = PipeWire is the sound system HushMic works with. HushMic needs it running, with its command line tools.
live-down-title = The HushMic microphone isn’t running.
live-down-body = HushMic keeps trying. If it doesn’t come back, the diagnostics show what is missing.
live-try-again = Try again
copy-diagnostics = Copy diagnostics
copy-diagnostics-busy = Collecting…
copy-diagnostics-done = Copied
live-troubleshooting = Troubleshooting guide

# Live: the last 10 seconds (a row under the picture: Replay and the hold
# button, and under them one line when there is something to say).
# The row's name for screen readers.
live-replay-group = The last 10 seconds
# The picture's name for screen readers while a replay plays: the take
# stands still with a playhead, and one of its lanes is heard.
live-pic-replay = The last 10 seconds, replaying through HushMic.
live-pic-replay-original = The last 10 seconds, replaying your original microphone.
# Replay's tooltip: the two join as one.
mic-st-listening-lead = HushMic keeps the last 10 seconds while Live is open.
# Replay and Hold's tooltip while they are off with no take yet (Live was
# just opened again).
mic-no-take-tip = Nothing to replay yet. Live keeps the last 10 seconds while it’s open.
mic-st-listening-body = Nothing is saved.
# Replay is disabled until the take is long enough: its button fills.
mic-st-collecting = Collecting the last 10 seconds…
mic-st-off = Replay works while HushMic is on.
mic-st-pipewire = Replay works again once PipeWire is back.
mic-st-down = Replay works again once the HushMic microphone is back.
mic-st-old-pipewire = Replay needs a newer PipeWire.
# A replay loops until Stop (Stop's tooltip).
mic-title-replaying = Playing on repeat. Stop ends it.
# Speakers, while its hold plays the original.
mic-sub-holding = Let go to hear HushMic again.
# After a tap of the hold button: the original stays on.
mic-sub-latched = Press again to hear HushMic.
# After a sequential take's button chose the original (mic-sub-latched
# follows).
mic-st-seq-chosen = Replay plays your original microphone.
mic-sub-connecting = The last 10 seconds start once HushMic hears your microphone.
mic-sub-old-pipewire = Noise suppression works as usual.
mic-note-changed-lead = Your settings changed,
mic-note-changed-body = so the last 10 seconds start over.
mic-note-sequential-lead = Same-moment switching isn’t possible for this take.
mic-note-sequential-body = Each side plays from the start.
mic-warn-playback = HushMic couldn’t play the take.
mic-warn-capture = The microphone stopped for a moment. Listening again.
# Replay's tooltip while the microphone has been silent for 3 s
# (the last 10 seconds may still hold speech, so it never says "none").
mic-take-empty = Replay waits for sound from your microphone.
# The level beside Live's strength, named for screen readers ("Level
# Sounds good"); a level that needs attention adds a notice.
mic-level = Level
mic-level-quiet-note = Very quiet.
mic-level-loud-note = Too loud.
mic-level-quiet-advice = Raise the input volume in your sound settings.
mic-level-loud-advice = Lower the input volume in your sound settings.
mic-replay = Replay the last 10 seconds
# The same button where the row is too narrow for the sentence; the
# sentence stays its tooltip and its name for screen readers.
live-replay-short = Replay 10 s
mic-stop = Stop
mic-hold = Hold to hear the original
# The hold button's description for screen readers: a tap keeps the
# original on, a hold compares while held.
mic-hold-desc = Press to switch, hold to compare.
mic-hold-space = Space does the same from anywhere on the page.
# The button for a take that can't switch at the same moment: it picks the
# side Replay plays.
mic-hold-seq = Hear the original
mic-hold-seq-desc = Press to switch sides. Each side plays from the start.

# Speakers: HushMic Speakers is an output device. What an app plays
# through it reaches the real speakers or headphones with the background
# noise removed (the other side of a call, a video). "Speaker cleaning" is
# the feature's name on its switch. The header says its state alone.
speakers-title-off = Speaker cleaning is off.
speakers-title-on = Removing noise from what you hear.
# On, and no app plays through HushMic Speakers yet.
speakers-title-ready = Ready.
# While "Hold to hear the original" is held or latched: the sound plays
# without cleaning.
speakers-title-holding = Playing the original.
# $output is the chosen output's name, e.g. "USB Headset".
speakers-title-unplugged = { $output } is unplugged.
# The same title when the output’s name doesn’t fit on its line.
speakers-title-unplugged-plain = Your output is unplugged.
speakers-title-paused = Speaker cleaning paused for now.
speakers-title-stopped = Speaker cleaning is stopped.
speakers-switch = Speaker cleaning
speakers-switch-desc = Adds HushMic Speakers to your outputs while it is on.
# The line at the top of the page: what it does, then how to use it.
speakers-about = Cleans the sound apps play, like the other side of a call.
speakers-route = Pick HushMic Speakers as the output in the app, or in your system’s sound settings.
speakers-hold-desc = Press to switch, hold to compare, on what plays now.
speakers-hold-off-tip = Works while speaker cleaning is on.
# The three cards. The eyebrows are set in capitals by the window.
speakers-output = Output
speakers-play-to = Play to
# $device: the output the system default is now, e.g. "Built-in Audio".
speakers-unplugged-note = Unplugged · playing on { $device }
# System default while HushMic Speakers itself is the system output: it
# plays on the real output ($device) that was the system output before.
speakers-system-default-self = Plays on { $device }: HushMic Speakers is the system output
speakers-delay = Sound arrives about 90 ms later than without cleaning.
speakers-cleaning = Cleaning
# The strength caption while the cleaner does not run: the switch is
# off, HushMic is off, it is starting, or it can't run now.
speakers-strength-applies = Applies when speaker cleaning is on.
speakers-strength-hushmic-off = HushMic is off. This applies when you turn it on.
speakers-strength-starting = Applies as soon as speaker cleaning has started.
speakers-strength-down = Applies once speaker cleaning runs again.
# Who plays through HushMic Speakers. $app, $app1, $app2 are app names.
speakers-playing-one = { $app } is playing here.
speakers-playing-anon = An app is playing here.
speakers-playing-two = { $app1 } and { $app2 } are playing here.
# $count is a number, 3 or more.
speakers-playing-many = { $count } apps are playing here.
speakers-playing-none = No app is playing here yet.
# Every app playing here: the line's tooltip and its name for screen
# readers from three apps on. $apps is the names with commas.
speakers-playing-all = Playing here: { $apps }
# Problems in the Status card: the lead runs into the body.
# The original plays (held or latched); the body says how to get the
# cleaning back (mic-sub-holding or mic-sub-latched).
speakers-holding-lead = Playing the original, without cleaning.
speakers-down-lead = HushMic Speakers can’t start.
# $output: the chosen output; $device: where the sound plays meanwhile.
speakers-unplugged-lead = { $output } is unplugged.
speakers-unplugged-body = Playing on { $device } until it’s back.
speakers-choose-output = Choose an output
# An app a restart of HushMic Speakers left on another output (it plays
# there uncleaned); one line per app. $app is its name, or "An app".
speakers-left-lead = { $app } left HushMic Speakers when it restarted.
speakers-left-body = Pick HushMic Speakers for it again.
speakers-busy-lead = Your computer is too busy for speaker cleaning.
speakers-busy-body = Sound plays as it is for now.
speakers-default-lead = HushMic Speakers is your system output.
speakers-default-body = Everything you hear is cleaned and arrives about 90 ms later, games and music too.

# Files: clean a recording (audio or video) with HushMic. One pass cleans
# each file; the strength, the hold and Save are mixes of that pass and the
# original, so nothing is cleaned again. The header is the title alone;
# what to do about a problem (files-help-*) is its row's tooltip.
files-title-empty = Clean a file.
# "Save all and quit" was chosen: the page's title until HushMic quits,
# with Cancel beside it
files-title-quitting = { $count ->
    [0] HushMic is quitting.
    [one] HushMic quits once 1 file is saved.
   *[other] HushMic quits once { $count } files are saved.
}
files-quit-cancel = Cancel
files-title-drag = Drop to clean them.
# $ext is the file's extension without the dot, e.g. "wma".
files-title-format = HushMic doesn’t open .{ $ext } files.
files-help-format = Most other audio and video files work.
files-title-no-space = There isn’t enough space on the disk.
files-help-no-space = Free some space, or save it somewhere else.
# $name is the file's name, e.g. "interview.m4a".
files-title-moved = { $name } moved.
files-help-moved = It moved while it was being cleaned. Nothing was written.
# The original moved after it was cleaned, before Save wrote its copy.
files-help-moved-save = It moved before it was saved. Nothing was written.
# A Save that failed: the file is cleaned, only its saving didn't happen.
# $folder is the folder's name, e.g. "Cleaned".
files-title-no-write = Can’t write to { $folder }.
files-help-no-write = It wasn’t saved. Choose another folder, or allow writing to this one.
# $format is the output's format, e.g. "M4A".
files-title-save-encode = Couldn’t write the { $format } file.
# The encoder (ffmpeg) refused to write this format; trying again can't
# help. $why is ffmpeg's own reason, in English.
files-help-save-encode-why = ffmpeg: { $why }. Nothing was saved.
files-help-save-encode = ffmpeg couldn’t write this format. Nothing was saved.
files-help-save-again = Nothing was saved. Try saving again.
files-title-save-read = Couldn’t read the original.
files-help-save-read = { $name } couldn’t be read again to save it. Check that it opens in another app.
# A Save that failed for a reason only the system names. $name is the
# file's name; $why is the system's short reason, e.g. "Input/output
# error" (only in the row's tooltip).
files-title-save-failed = Couldn’t save { $name }.
files-help-save-failed = { $why }. Nothing was saved. Try saving again.
# Found as the file is added. $name is the file's name.
files-title-incomplete = { $name } is incomplete.
files-help-incomplete = This file is incomplete (the recording was probably cut off). HushMic can’t read it.
files-row-incomplete = Incomplete
files-title-not-media = Not a recording: { $name }
# A file with a recording's name (.wav, .mp4) whose bytes aren't one.
files-title-broken = Can’t read this file.
files-help-broken = It may be damaged or incomplete.
files-row-broken = Can’t be read
files-help-not-media = This isn’t an audio or video file.
files-row-not-media = Not audio or video
files-title-empty-file = { $name } is empty.
files-help-empty = There is nothing in it (0 bytes).
files-title-unreadable = HushMic can’t read { $name }.
files-help-unreadable = Check that it opens in another app, then choose it again.
files-title-no-sound = This video has no sound track.
files-help-no-sound = There is nothing in it to clean.
files-title-silent = There’s no sound in this file.
files-help-silent = It is silent all the way through.
files-title-engine = The cleaning engine didn’t start.
files-help-engine = Nothing was written. Try again in a moment.
# A pass that stopped (an error of its own, or its process ended).
files-title-stopped = Cleaning stopped before the end.
files-help-stopped = Nothing was written. Try again.
# The second stop in a row.
files-help-stopped-again = It stopped again. Restart HushMic, then try once more.
# $gb is a number of gigabytes with one decimal, e.g. "0.3".
files-title-cache = Cleaning this file needs about { $gb } GB free.
files-help-cache = It is used while the file is open. Free some space, then try again.
# $name is the file's name, e.g. "interview.m4a".
files-title-damaged = Part of { $name } is damaged.
# $time is a length, e.g. "3:12".
files-help-damaged = HushMic can clean the first { $time }.
# The figure's button for the next step.
files-try-again = Try again
files-clean-first = Clean the first { $time }
files-title-original = You are hearing the original.
files-title-original-on = The original is on.
files-title-caught-up = Cleaning ahead…
# The player caught up with a pass that gets no CPU (a busy computer).
files-title-caught-up-stalled = Waiting for the cleaning to catch up.
files-title-playing = You are hearing HushMic.
# $name is the cleaned file's name, e.g. "interview (cleaned).m4a".
files-title-saved = Saved { $name }.
# Saved, then the strength changed: $strength is the one it was saved at,
# e.g. "Maximum" (strength-maximum).
files-title-saved-at = { $name } was saved at { $strength }.
files-title-saved-folder-gone = Saved next to the original: the chosen folder is gone.
files-title-saved-folder-silent = Saved next to the original: the chosen folder didn’t answer.
# $folder is a folder's short name, e.g. "Recordings/Cleaned".
files-title-saved-in = Saved in { $folder }.
# $name is the file's name, e.g. "lecture-03.mp4".
files-title-cleaned = { $name } is cleaned.
# The row's Save button's tooltip: $name is the cleaned file's name, e.g.
# "interview (cleaned).m4a".
files-save-tip = Save as { $name }
files-save-at-tip = Save { $strength } as { $name }
# $name is the file's name, e.g. "lecture-03.mp4".
files-title-cleaning = Cleaning { $name }.
# $name is the file's name, e.g. "lecture-03.mp4".
files-title-waiting = { $name } is waiting its turn.
# $other is the file that cleans meanwhile, e.g. "interview.m4a".
files-title-waiting-for = { $name } is waiting its turn: { $other } is cleaning.
# The picture, for screen readers. $name is the file's name.
files-picture = { $name }: the original above, the cleaned sound below.
# The captions of the picture's two lanes; the file's name follows the
# first, the cleaned copy's name the second.
files-lane-original = Original
files-lane-cleaned = Cleaned
# In the two empty lanes, before the first file is added.
files-lane-original-empty = Your recording
files-lane-cleaned-empty = The same recording, without the noise
# Beside Play: where the file is and how long it is, e.g. "1:38 / 12:40".
files-time = { $at } / { $total }
files-choose = Choose files…
files-play = Play
files-pause = Pause
files-position = Position in the file
# The time line's value for screen readers, e.g. "1:23 of 12:40".
files-position-value = { $at } of { $total }
# The toolbar over the list (its name for screen readers, and its word
# with no file yet); with several files, files-bar-all.
files-files = Files
files-bar-all = All files
# Under "Choose files…" with no file yet: files can also be dragged in.
files-formats = Or drop them here. Audio or video: WAV, MP3, M4A, FLAC, Opus, MP4, MKV and more.
files-add = Add files…
# The × on each file's row (its tooltip); files-remove-name is its name.
files-remove = Remove
files-remove-name = Remove { $name }
files-choose-again = Choose it again…
# The strength's tooltip when there is more than one file: it is the
# selected file's, and the files added after take it.
files-every-file = For this file and the files you add next.
# Under the player at Strong, the strength Files starts at.
files-strength-strong = Most of the noise is gone. Files start at Strong to keep the room tone.
# The strength's name for screen readers.
files-strength-name = Strength, for this file and the files you add next
files-save = Save
# A row's Save, for screen readers: $name is the cleaned file's name.
files-save-name = Save { $name }
files-save-all = Save all
# Removes every file; cleaned copies already saved stay on the disk.
files-clear-all = Clear all
# Quit while files clean or save: the quit sheet's last sentence, and its
# safe answer.
files-sub-quit = Files that aren’t saved yet are cleaned again when you add them next time.
files-keep-cleaning = Keep cleaning
# The quit sheet's button while files would be lost: they are saved (each
# once cleaned), then HushMic quits.
files-save-all-and-quit = Save all and quit
# A short note after a drop or a choice (a toast). $name is a file's name,
# $folder a folder's, $count how many files.
files-note-not-media = { $name } isn’t an audio or video file.
# Two files: $a and $b are their names.
files-note-not-media-two = { $a } and { $b } aren’t audio or video files.
# More: $names is the first two, e.g. "notes.txt, list.pdf", and $count how
# many others.
files-note-not-media-many = { $names } and { $count ->
    [one] 1 more
   *[other] { $count } more
} aren’t audio or video files.
files-note-already = { $name } is already in the list.
files-note-already-many = { $count ->
    [one] 1 file is already in the list.
   *[other] { $count } files are already in the list.
}
# A file whose name says HushMic cleaned it already: added all the same.
files-note-cleaned = { $name } looks cleaned already. It’s cleaned again.
files-note-empty-folder = { $folder } has no audio or video files.
files-note-skipped-cleaned = Left out { $name }: it’s the cleaned copy of a file in the drop.
files-note-skipped-cleaned-many = Left out { $count } cleaned copies of files in the drop.
files-note-capped = Added the first { $count } files.
# The toast after Clear all, with an Undo button for a few seconds.
files-cleared = { $count ->
    [one] 1 file cleared
   *[other] { $count } files cleared
}
files-undo = Undo
# The toast after a row's × on a file not saved yet (with Undo). $name is
# the file's name.
files-removed = { $name } removed
# Quit while cleaned files aren't saved: before files-sub-quit. $count is
# how many.
files-quit-unsaved = { $count ->
    [one] 1 cleaned file isn’t saved.
   *[other] { $count } cleaned files aren’t saved.
}
# $time is files-about-minutes.
files-ready-in = Ready in { $time }
# A row's state below a minute (the row is narrow).
files-ready-under-a-minute = Ready in under 1 min
# A row's tooltip when the file was read below its quality (HE-AAC sound
# without ffmpeg: only its basic layer decodes).
files-reduced-tip = Cleaned at reduced quality. Install ffmpeg for full quality.
# A recording with more than one sound track (OBS writes one per source):
# one is cleaned (the microphone's, by its name, else the first), the
# others are kept as they are. $n is its number, $count how many there are,
# $title its name in the file (e.g. "Mic/Aux").
files-cleans-track = Cleans track { $n } of { $count }
files-cleans-track-titled = Cleans track { $n } · { $title }, of { $count }
files-tracks-copied = The other sound tracks are kept as they are.
# The choices of the track to clean (a menu).
files-track = Track { $n }
files-track-titled = Track { $n } · { $title }
# The track button's name for screen readers.
files-track-change = The sound track to clean
files-tracks-dropped = Only the first of { $count } sound tracks is kept. Install ffmpeg to keep them all.
# A row being saved, before its percent ("Saving · 39 %").
files-saving = Saving
# How far a file's cleaning (or its save) is, after its state words in the
# list: $n is a whole number from 0 to 100.
files-percent = { $n }%
# Its spaces don't break ("about 6 min" stays on one line).
files-about-minutes = about{"\u00A0"}{ $n }{"\u00A0"}min
files-saves-when-cleaned = Saves when cleaned
files-cancel = Cancel
# A row's Cancel, for screen readers: $name is the cleaned file's name.
files-cancel-name = Cancel saving { $name }
files-show-in-folder = Show in folder
# A row's button for screen readers: $action is its label, $name the file's.
files-action-name = { $action }: { $name }
files-save-elsewhere = Save elsewhere…
files-choose-another-folder = Choose another folder…
files-save-again = Save again
# A file's state in its row: a label, no full stop.
files-row-cleaning = Cleaning
# Under the player while the selected file cleans, over what the strength
# does: how far (files-percent), and how long is left (files-about-minutes).
files-cleaning-left = Cleaning… { $percent } · ready in { $time }
files-cleaning-soon = Cleaning… { $percent } · ready in under 1 min
files-cleaning-at = Cleaning… { $percent }
# Under the player: a file paused for a call, and how far it got; on one
# line where it fits, else its two halves (files-paused-at over
# files-continues-after-call).
files-paused-call = Paused at { $percent } · continues after your call
files-paused-at = Paused at { $percent }
files-continues-after-call = Continues after your call
# Under the player: a file that stopped partway and waits its turn (another
# file was played); playing it cleans it on. Whole, or its two halves
# (files-waiting-at over files-play-to-continue). During a call,
# files-waiting-at alone.
files-waiting-play = Waiting at { $percent } · play it to continue
files-waiting-at = Waiting at { $percent }
files-play-to-continue = Play it to continue
# A cleaned or saved file's state with its own strength, e.g. "Cleaned ·
# Strong" ($strength is strength-strong and so on).
files-row-cleaned-at = Cleaned · { $strength }
files-row-saved-strength = Saved · { $strength }
# A saved row's tooltip: the file its Save wrote, e.g. "Saved as interview (cleaned, Gentle).m4a."
files-row-saved-as = Saved as { $name }.
# Cleaning waits while an app records HushMic on a busy computer: the
# row's short state, the whole sentence (its tooltip and the title) with
# the app's name ($app, e.g. "Zoom") or without, and the button.
files-row-yielded = Paused for your call
files-paused-for = Paused while { $app } uses your microphone. Your call comes first.
files-paused = Paused while an app uses your microphone. Your call comes first.
files-clean-anyway = Clean anyway
files-row-waiting = Waiting
# A waiting file's action: it cleans before the other waiting files.
files-clean-next = Clean next
# Saved at another strength than the page's now. $strength is its word,
# e.g. "Maximum" (strength-maximum).
files-row-saved-at = Saved at { $strength }
# The row's Save at the page's strength now, e.g. "Save Strong".
files-save-at = Save { $strength }
files-row-saves-now = Saves now
files-row-format = This file type doesn’t open here
files-row-no-space = Not saved: the disk is full
files-row-moved = Moved while it was being cleaned
files-row-moved-save = Moved before it was saved
files-row-no-write = Not saved: the folder can’t be written
# $format is the output's format, e.g. "WAV".
files-row-save-encode = Not saved: the { $format } encoder failed
files-row-save-read = Not saved: the original couldn’t be read
files-row-save-failed = Not saved
files-row-unreadable = Can’t be read
# A file of 0 bytes.
files-row-empty = Empty file
files-row-no-sound = No sound track
files-row-silent = Silent
files-row-engine = Didn’t start
files-row-stopped = Stopped before the end
files-row-cache = Needs { $gb } GB free
# $time is how much can be cleaned, $length how long the file says it is
# (files-row-damaged-at when it doesn't say).
files-row-damaged = Damaged after { $time } of { $length }
files-row-damaged-at = Damaged after { $time }

# Shortcuts.
sc-title = Shortcuts
# The row with the button that opens the desktop's shortcut window.
sc-keys-kept = Your desktop keeps the keys
sc-change = Change shortcuts
sc-setup = Set up shortcuts
sc-open-settings = Open System Settings
sc-unavailable-title = This desktop doesn’t let apps register shortcuts.
sc-unavailable-body = Add these commands as custom shortcuts in your keyboard settings.
# Under sc-unavailable-body: what holding a key needs, then a link to the recipe.
sc-unavailable-hold = Push to talk and push to mute need a key’s press and its release bound apart, as sway, i3 and other window managers can.
sc-hold-recipe = How to bind press and release
sc-toggle-mute = Toggle mute
sc-toggle-mute-desc = Press to mute, press again to unmute.
sc-toggle-bypass = Toggle noise suppression
sc-toggle-bypass-desc = Press to send your raw microphone, press again to clean it.
sc-push-to-talk = Push to talk
sc-push-to-talk-desc = Your microphone is live only while you hold the key. Letting go mutes it.
sc-push-to-mute = Push to mute
sc-push-to-mute-desc = Silences your microphone while you hold the key, like a cough button.
sc-not-set = Not set
sc-not-reported = Not shown by your desktop
# Once under the rows when the desktop shows none of the keys it set.
sc-not-reported-all = Your desktop doesn’t show which keys it set. Change shortcuts opens them.
# Where shortcuts can't be registered, push to talk and push to mute name
# the command for the key's press and the one for its release.
sc-on-press = Press
sc-on-release = Release
sc-legend-held = key held
sc-legend-clean = apps hear your cleaned voice
sc-legend-raw = raw microphone
sc-legend-silence = silence
# The legend for screen readers; $items are the four words above.
sc-legend-name = Legend: { $items }
sc-mute-note = Mute only silences the HushMic microphone. Apps that use your microphone directly still hear you.

# Settings.
set-title = Settings
set-general = General
set-autostart = Start HushMic when you log in
set-notifications = Notifications
set-notifications-cap = When something needs you, and when your microphone comes back.
# Settings › Apps (round 3b: the default microphone moved here from Live).
set-apps = Apps
set-default-mic = Use HushMic as the default microphone
set-default-mic-cap = Apps that follow the system default then use HushMic without setup. In other apps, choose the microphone named HushMic.
# $device: where the user moved the system default.
set-default-released-title = HushMic is no longer the default microphone.
set-default-released-body = Apps now use { $device } directly.
set-default-again = Make it the default again
set-appearance = Appearance
set-appearance-system = System
set-appearance-dark = Dark
set-appearance-light = Light
set-sound = Noise suppression
set-model = Model
set-model-cap = Light needs less of your processor. HushMic steps down to Light by itself when your computer is busy.
# A switch: the model keeps to the voice it last let through, so voices
# from farther away stay out even after the user has paused.
set-voice-hold = Stay on my voice
set-voice-hold-cap = Keeps voices farther from the microphone out, also while you pause. People beside you can sound quieter.
set-tray = Tray
set-tray-show = Show tray icon
set-tray-show-cap = Takes effect the next time HushMic starts.
set-tray-icon = Tray icon
set-tray-icon-cap = Auto uses the monochrome icon on KDE and GNOME, which recolor it to match your panel, and the color icon elsewhere.
set-tray-icon-auto = Auto
set-tray-icon-color = Color
set-tray-icon-mono = Monochrome
set-no-tray-title = No tray on this desktop.
set-no-tray-body = On GNOME, the AppIndicator and KStatusNotifierItem Support extension adds one. HushMic keeps running without it.
set-mics = Microphones
set-mics-cap = Each microphone keeps its own strength and model. The others share these.
# A microphone's own settings, e.g. "Maximum · Quality".
set-mic-sound = { $strength } · { $model }
set-mic-custom = Custom
# The microphone in use: its set-mic-sound, e.g. "Maximum · Quality · in use".
set-mic-in-use = { $sound } · in use
set-mic-reset = Reset
# The Reset button's name for screen readers, e.g. "Reset RODE NT-USB".
set-mic-reset-name = Reset { $mic }
set-mics-others = Other microphones
# Settings › Files: where Save puts cleaned files.
set-files = Files
set-files-save = Save cleaned files
set-files-next = Next to the original
set-files-folder = In a folder…
set-files-next-cap = Saved again at another strength, the copy’s name says it: “(cleaned, Gentle)”.
# While the folder chooser is open, before a folder is chosen.
set-files-choosing = Choose the folder in the window that opened.
set-files-change = Change…
# The Change… link's name for screen readers.
set-files-change-name = Change the folder for cleaned files
# The Flatpak: no "next to the original"; the folder asked for at the first Save.
set-files-sandbox-cap = The Flatpak can only save into a folder you choose (the first Save asks). Choose the recordings’ own folder to keep each copy beside its original.
set-files-unset = Not chosen yet
set-files-choose = Choose…
# "In a folder…" selected, no folder chosen yet: opens the folder chooser.
set-files-choose-folder = Choose a folder…
set-files-choose-name = Choose a folder for cleaned files
# Help text under the save place, not a setting (the file manager's entry
# is named "Clean with HushMic", packaging/hushmic-clean.desktop).
set-files-open-with-help = Your file manager also offers “Clean with HushMic” for audio and video files.
set-advanced = Advanced
set-engine = Model engine
set-engine-cap = How HushMic runs its noise model. Automatic picks the lightest way for your processor. ONNX Runtime uses more CPU; choose it only if Automatic gives you trouble. A change restarts the microphone for a moment.
set-engine-auto = Automatic
set-engine-onnx = ONNX Runtime
set-terminal = Use HushMic from a terminal
set-headless = Run without a window
set-about = About
set-version = Version { $version } · Real-time noise suppression for Linux
set-website = Website
set-report = Report a problem
set-licenses = Licenses
set-credits = Noise model: DPDFNet · Fonts: Geist and Geist Mono (SIL Open Font License) · Icons: Lucide (ISC) · Translated on Weblate
set-licenses-title = Licenses
set-licenses-close = Close
# About › Licenses: the section of the Rust libraries HushMic is built from.
set-licenses-crates = Rust crates
# Under an MPL-2.0 crate’s license: where its source code is.
set-licenses-source = Source code: { $url }

# First run.
# The steps' name for screen readers, e.g. "Step 2 of 3: Hear it work".
fr-steps-name = Step { $n } of { $total }: { $step }
fr-step-mic = Microphone
fr-step-hear = Hear it work
fr-step-apps = Use it in your apps
fr-skip = Skip
fr-back = Back
fr-files-only = Only here to clean recordings?
fr-files-go = Go to Files
fr-continue = Continue
fr-done = Done
fr-mic-title = Which microphone should HushMic clean?
fr-mic-sub = You can change it any time on Live.
fr-mic-default-note = Follows your sound settings. Now: { $device }
fr-mic-usb = USB microphone
fr-mic-builtin = Your computer’s microphone
fr-mic-headset = Headset microphone
fr-mic-bluetooth = Bluetooth microphone
fr-mic-other = Microphone
fr-level = Level
fr-level-cap = Speak: the bar moves when this microphone hears you.
fr-level-old = The level shows on newer PipeWire versions. HushMic works either way.
fr-engine-ready = HushMic is ready in the background.
fr-engine-starting = HushMic is starting in the background…
fr-engine-off = HushMic is off. You can turn it on in the next step.
fr-hear-title = Talk, then type or tap the desk.
fr-hear-sub = This is what your apps hear. The noise stays out.
fr-hear-bypass-sub = Your microphone goes through as it is. Turn it on to hear it work.
# The adaptive engine paused noise suppression for a while
# (the computer is too busy); the title is live-title-unfiltered.
fr-hear-unfiltered-sub = Your computer is busy, so noise suppression is paused for now. It comes back by itself.
fr-hear-muted-title = HushMic is muted.
fr-hear-muted-sub = Unmute it to hear it work.
fr-hear-off-sub = Turn it on to hear it work.
fr-hear-pic = Your voice goes through to your apps; the background noise stays behind.
# Step 2: plays the last 10 seconds, cleaned, until Stop (mic-stop).
fr-hear-play = Play the last 10 seconds
fr-hear-later-live = Later, Live lets you replay the last 10 seconds and hear your original microphone at the same word.
fr-apps-title = Use HushMic in your apps.
fr-apps-sub = In Zoom, Google Meet, Teams or any call or recording app, open its audio settings and pick HushMic as the microphone.
# A picture of Zoom's audio settings (not a control), with its caption.
fr-apps-example = Zoom · Audio
fr-apps-example-cap = In Zoom it looks like this.
fr-apps-example-name = In Zoom it looks like this: Microphone, HushMic.
# The switch under the example is set-default-mic, as in Settings › Apps.
fr-apps-default-cap = Turn this on if you’re not sure. Zoom, Meet and Teams then use HushMic automatically.
fr-apps-waiting = Waiting for an app to use HushMic…
# $app is the app's own name, e.g. "Firefox".
fr-apps-using = { $app } is using HushMic.
fr-apps-using-anon = An app is using HushMic.

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
notify-window-failed-body = HushMic could not open its window on this display.
# "hushmic miccheck" is a command: keep it as-is.
notify-window-failed-miccheck = To test your microphone without the window, run: hushmic miccheck

notify-no-tray-title = HushMic is running without a tray icon
notify-no-tray-body = No system tray was found. Start HushMic again to open its window, or use hushmic status, mode and config in a terminal. On GNOME, the 'AppIndicator and KStatusNotifierItem Support' extension adds a tray.

notify-chain-failed-summary = HushMic could not start the virtual microphone
notify-chain-stuck-summary = HushMic keeps losing the virtual microphone
notify-chain-stuck-body = Re-creating it keeps failing — is PipeWire running? Run `hushmic --doctor` from a terminal for details.
notify-chain-flapping-summary = HushMic keeps restarting the virtual microphone
notify-chain-flapping-body = It went down and was re-created several times in the last few minutes — the audio setup may be unstable. Run `hushmic --doctor` from a terminal for details.
notify-running-again-summary = HushMic is running again
notify-running-again-body = The virtual microphone is back up.
notify-speakers-failed-summary = HushMic could not start HushMic Speakers
notify-speakers-failed-body = It keeps failing to start. Run `hushmic --doctor` for details.
notify-speakers-running-again-summary = HushMic Speakers is running again
notify-speakers-running-again-body = What you hear is cleaned again.

# After Off or Quit while an app used HushMic.
notify-direct-off-summary = HushMic is off
notify-direct-quit-summary = HushMic has quit
# $apps names them (one app's name, notify-apps-two or notify-apps-many),
# $count is how many, $mic is the microphone they hear now, e.g.
# "Zoom now hears RODE NT-USB directly, without noise suppression."
notify-direct-body = { $count ->
    [one] { $apps } now hears { $mic } directly, without noise suppression.
   *[other] { $apps } now hear { $mic } directly, without noise suppression.
}
# The button on "HushMic is off": noise suppression on again.
notify-turn-on = Turn on
# The buttons on "No sound from …": $mic is the one other microphone.
notify-use-mic = Use { $mic }
notify-open-hushmic = Open HushMic
notify-apps-two = { $app1 } and { $app2 }
# $count is 3 or more.
notify-apps-many = { $count } apps
notify-your-mic = your microphone
notify-mic-stuck-summary = HushMic's microphone isn't connecting
notify-mic-stuck-body = PipeWire doesn't connect this microphone to HushMic, even after three restarts. Unplug it and plug it back in, or choose another microphone in HushMic's window.
notify-reroute-summary = Another app is re-routing HushMic's microphone
notify-reroute-body = A running audio tool (EasyEffects?) keeps redirecting HushMic's input to itself. In EasyEffects, disable "Process All Input Streams" (or exclude hushmic_input), then restart HushMic.

# The user (or another app) switched the system default microphone away
# from HushMic while "Use HushMic as the default microphone" was on; $device is
# the new default's name. Settings › Apps then offers "Make it the default
# again".
notify-default-released-summary = HushMic is no longer the default microphone
notify-default-released-open = Apps now use { $device } directly. To make HushMic the default again, open its Settings › Apps.

# $mic is the chosen microphone, $standin the one HushMic uses meanwhile
# (or notify-system-default).
notify-mic-fallback-body = { $mic } was disconnected. HushMic uses { $standin } for now.
notify-mic-return-body = { $mic } is back. HushMic switched back to it from { $standin }.
# "System default" moved to another microphone while an app uses HushMic.
notify-default-switched-body = HushMic now cleans { $new }, the new default microphone (it was { $old }).
notify-system-default = the system default

notify-shortcuts-title = HushMic shortcuts
notify-shortcuts-body = This desktop cannot open the shortcut editor from here. Look for HushMic in your system's keyboard shortcut settings to change the keys.

# The first time the user closes the window (sent once): closing does not
# stop HushMic.
notify-closed-summary = HushMic is still running
notify-closed-body = Open it again from the tray or your app menu.
# A setting changed in the window took effect, but writing it to the
# config file failed (it is lost at the next start). The body is the
# technical reason ("could not write <path>: <why>").
notify-settings-unsaved-summary = HushMic couldn't save your settings
# Files cleaned while the window was closed or in the background, not
# saved yet: with Save all (Save for one) and Show.
notify-files-ready = { $count ->
    [one] 1 recording is ready.
   *[other] { $count } recordings are ready.
}
notify-files-ready-body = { $count ->
    [one] It isn’t saved yet.
   *[other] They aren’t saved yet.
}
notify-files-save = Save
notify-files-save-all = Save all
# Every one of them was saved (Save all, or a Save asked while it cleaned).
notify-files-saved = { $count ->
    [one] Saved 1 recording.
   *[other] Saved { $count } recordings.
}
# Save all from the note: some couldn't be saved (the window says why).
notify-files-not-saved = { $count ->
    [one] 1 recording wasn’t saved.
   *[other] { $count } recordings weren’t saved.
}
notify-files-show = Show
# When every one of them was saved: where.
notify-files-saved-next = { $count ->
    [one] Saved next to the original.
   *[other] Saved next to the originals.
}
# $folder is a folder's short name, e.g. "Recordings/Cleaned".
notify-files-saved-in = Saved in { $folder }.
notify-files-show-in-folder = Show in folder
# The file chooser's filter for audio and video files.
files-chooser-recordings = Recordings
# The file chooser's other filter: every file, whatever its type.
files-chooser-all = All files
# The folder chooser's title, asked once in the Flatpak (Save can't write next to the originals there).
files-choose-folder = Choose where cleaned files go

notify-autostart-denied-title = Autostart was not allowed
notify-autostart-denied-body = The desktop denied HushMic's autostart request. You can allow it in your system settings under application permissions / background apps.

## The last 10 seconds: the result of a replayed take and the level.

miccheck-quieter-much = Much quieter
miccheck-quieter-clearly = Clearly quieter
miccheck-quieter-slightly = Slightly quieter
miccheck-quieter-same = About the same
# A measured difference in decibels, after the word; $db is a whole number.
miccheck-db = ({ $db } dB)
miccheck-level-waiting = Say a few words
miccheck-level-good = Sounds good
miccheck-level-quiet = Very quiet
miccheck-level-loud = Too loud
miccheck-level-silent = No sound
