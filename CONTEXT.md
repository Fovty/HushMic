# HushMic

HushMic provides a virtual microphone that preserves the user's speech while reducing unwanted background sound.

## Language

**Enrolled speaker**:
The person whose voice should remain audible when pause protection is enabled.
_Avoid_: First speaker, loudest speaker

**Enrollment**:
A deliberate session in which the intended speaker provides a clean voice sample to establish a voice reference.
_Avoid_: Automatic first-voice selection

**Voice reference**:
A fixed representation of the enrolled speaker's vocal characteristics that identifies the same person across pauses, restarts, and microphone changes, retained until explicit replacement or deletion.
_Avoid_: Loudness baseline, microphone profile

**Pause protection**:
Suppression of background speech while the enrolled speaker is absent. It does not imply separation of people speaking simultaneously.
_Avoid_: Speaker separation

**Microphone profile**:
Preferred denoising settings associated with a particular microphone.
_Avoid_: Voice reference, speaker profile
