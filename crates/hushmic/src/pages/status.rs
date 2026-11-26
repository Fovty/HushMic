//! What HushMic does now, in the rail's words: the Mark's state glyph and
//! the word that names it (its tooltip and screen-reader name).

use crate::appstate::{AppState, Mode, Phase};
use crate::diagnostics::EngineTier;
use crate::tr;
use crate::ui::paint::mark::MarkState;

#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    /// The state's word ("On", "Noise suppression off", …).
    pub word: String,
    pub mark: MarkState,
}

impl Status {
    /// The rail's Mark: its tooltip and screen-reader name.
    pub fn mark_name(&self) -> String {
        if self.mark == MarkState::Error {
            tr!("mark-needs-attention")
        } else {
            tr!("rail-mark-name", state = self.word.clone())
        }
    }
}

/// HushMic can't work at all: PipeWire is unreachable, or the chain is down
/// and not coming back by itself right now ("Needs attention").
pub fn stopped(s: &AppState) -> bool {
    !s.pipewire.reachable || (s.mode != Mode::Off && s.engine.phase == Phase::Down)
}

pub fn status(s: &AppState) -> Status {
    let st = |word: String, mark| Status { word, mark };
    if stopped(s) {
        return st(tr!("pill-stopped"), MarkState::Error);
    }
    if s.mode == Mode::Off {
        return st(tr!("pill-off"), MarkState::Off);
    }
    match s.engine.phase {
        Phase::Starting | Phase::Off => return st(tr!("pill-starting"), MarkState::Bypass),
        Phase::Reconnecting => return st(tr!("pill-reconnecting"), MarkState::Bypass),
        Phase::Ready | Phase::Down => {}
    }
    match s.mode {
        Mode::Mute => st(tr!("pill-muted"), MarkState::Muted),
        Mode::Bypass => st(tr!("pill-bypass"), MarkState::Bypass),
        _ if s.engine.tier == Some(EngineTier::Passthrough) => {
            st(tr!("pill-unfiltered"), MarkState::Bypass)
        }
        _ => st(tr!("pill-on"), MarkState::Active),
    }
}

/// Noise suppression is on: the switch's position. While muted it shows
/// where Unmute returns to.
pub fn suppressing(s: &AppState) -> bool {
    match s.mode {
        Mode::Suppress => true,
        Mode::Mute => s.unmute_to == Mode::Suppress,
        _ => false,
    }
}

/// HushMic filters what apps hear right now (the pages without the Live
/// picture light the window's top edge then).
pub fn filtering(s: &AppState) -> bool {
    !stopped(s)
        && s.mode == Mode::Suppress
        && s.engine.phase == Phase::Ready
        && s.engine.tier != Some(EngineTier::Passthrough)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appstate::mock::{state, Scenario};

    #[test]
    fn the_mark_follows_the_boards() {
        crate::i18n::pin_english();
        let cases = [
            (Scenario::OnInCall, "On", MarkState::Active),
            (Scenario::OnNoApp, "On", MarkState::Active),
            (Scenario::Bypass, "Noise suppression off", MarkState::Bypass),
            (Scenario::Muted, "Muted", MarkState::Muted),
            (Scenario::LightForNow, "On", MarkState::Active),
            (Scenario::UnfilteredForNow, "Paused", MarkState::Bypass),
            (Scenario::Starting, "Starting", MarkState::Bypass),
            (Scenario::Reconnecting, "Reconnecting", MarkState::Bypass),
            (Scenario::PipewireUnreachable, "Stopped", MarkState::Error),
            (Scenario::NeedsAttention, "Stopped", MarkState::Error),
            (Scenario::Off, "Off", MarkState::Off),
        ];
        for (sc, word, mark) in cases {
            let s = status(&state(sc));
            assert_eq!((s.word.as_str(), s.mark), (word, mark), "{sc:?}");
        }
        assert_eq!(
            status(&state(Scenario::OnInCall)).mark_name(),
            "HushMic: On"
        );
        assert_eq!(
            status(&state(Scenario::NeedsAttention)).mark_name(),
            "HushMic needs attention"
        );
    }

    #[test]
    fn the_switch_shows_where_unmute_returns() {
        let mut s = state(Scenario::Muted);
        assert!(suppressing(&s));
        s.unmute_to = Mode::Bypass;
        assert!(!suppressing(&s));
        assert!(!filtering(&state(Scenario::Muted)));
        assert!(filtering(&state(Scenario::OnNoApp)));
        assert!(!filtering(&state(Scenario::UnfilteredForNow)));
    }
}
