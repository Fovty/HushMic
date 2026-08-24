tray-title = HushMic
tray-title-bypass = HushMic (Bypass)
tray-title-muted = HushMic (stumm)
tray-title-error = HushMic (Fehler)

tray-test-mic = Mikrofon testen…
tray-test-running = Mikrofontest läuft…

tray-mode = Modus
tray-mode-suppress = Rauschunterdrückung
tray-mode-bypass = Bypass
tray-mode-mute = Stumm
tray-mode-off = Aus

tray-microphone = Mikrofon
tray-system-default = Systemstandard
tray-mic-unplugged = (nicht angeschlossen – Systemstandard wird verwendet)
tray-mic-unavailable = (nicht verfügbar)

tray-model = Modell
tray-model-dpdfnet8 = Hohe Qualität (dpdfnet8)
tray-model-dpdfnet2 = Leicht / CPU-schonend (dpdfnet2)

tray-strength = Unterdrückungsstärke
tray-attn-maximum = Maximal
tray-attn-strong = Stark (24 dB)
tray-attn-medium = Mittel (12 dB)
tray-attn-light = Leicht (6 dB)

tray-set-default = Als Standardmikrofon festlegen
tray-shortcuts-setup = Tastenkürzel einrichten…
tray-shortcuts-change = Tastenkürzel ändern…
tray-autostart = Beim Anmelden starten
tray-about = Über HushMic…
tray-quit = Beenden

ab-window-title = HushMic – Mikrofontest
ab-heading = Live-A/B-Mikrofontest
ab-subheading = Vergleichen Sie das rohe Mikrofonsignal mit der von HushMic gefilterten Ausgabe

ab-channel-raw = Rohes Mikrofonsignal
ab-channel-raw-desc = Unbearbeitetes Signal direkt von Ihrem Gerät
ab-channel-filtered = Von HushMic gefiltert
ab-channel-filtered-desc = Was andere Anwendungen empfangen
ab-playing-badge = WIEDERGABE

ab-status-listening = Mithören
ab-status-recording = Aufnahme
ab-status-playback = Wiedergabe
ab-status-stopped = Angehalten
ab-status-no-input = Kein Eingang
ab-sample-ready = Aufnahme bereit
ab-not-monitoring = Kein Mithören

ab-timeline-live = Live-Mithören
ab-timeline-recording = Aufnahme läuft – noch { $secs } s
ab-timeline-playing-raw = Wiedergabe der Rohaufnahme
ab-timeline-playing-filtered = Wiedergabe der gefilterten Aufnahme
ab-timeline-input-unavailable = Eingang nicht verfügbar

ab-record-10s = 10-Sekunden-Aufnahme starten
ab-record-new = Neue Aufnahme starten
ab-play-raw = Ungefiltert abspielen
ab-play-filtered = Gefiltert abspielen
ab-go-live = Live schalten
ab-cancel = Abbrechen
ab-stop = Stopp
ab-retry-detection = Erkennung wiederholen

ab-press-prefix = Drücken Sie{" "}
ab-press-suffix = , um zur geteilten Ansicht zurückzukehren. Es wird nichts gespeichert, bis Sie eine Aufnahme starten.

ab-summary-hint = Starten Sie eine 10-Sekunden-Aufnahme, um die Reduktion der Hintergrundgeräusche und den Erhalt der Stimme zu messen
ab-card-background = HINTERGRUNDGERÄUSCHE
ab-card-voice = STIMME
ab-raw-db = Roh { $db }
ab-filtered-db = Gefiltert { $db }
ab-silent = still
ab-reduction-at-least = ≥ { $db }
ab-quieter = leiser
ab-preserved = Erhalten
ab-voice-unmeasurable = zum Messen deutlich sprechen

ab-toast-silent-mic = Vom Mikrofon kommt kein Audio an – falls es ein Funk-Headset ist, prüfen Sie, ob es eingeschaltet ist.
ab-toast-processed-died = Der verarbeitete Stream wurde unerwartet beendet – HushMic startet ihn möglicherweise gerade neu; versuchen Sie es gleich mit „Live schalten“.
ab-toast-abort-stream = Aufnahme abgebrochen – der Aufnahme-Stream wurde unerwartet beendet
ab-toast-abort-mic-changed = Aufnahme abgebrochen – das Mikrofon hat sich geändert

ab-settle-title = Verbindung mit Ihrem Mikrofon wird hergestellt…
ab-settle-body = HushMic startet seine Audiokette. Das dauert ein paar Sekunden.
ab-no-input-title = Kein Mikrofonsignal
ab-no-input-body = HushMic empfängt kein Audio von einem Mikrofon. Wählen Sie Ihr Mikrofon im Menü „Mikrofon“ des Tray-Symbols (und prüfen Sie, ob es angeschlossen ist und PipeWire läuft), und versuchen Sie es dann erneut.

about-window-title = Über HushMic
about-version = Version { $version }
about-tagline = Rauschunterdrückung in Echtzeit für Linux
about-model = Rauschmodell: DPDFNet
about-copy-diagnostics = Diagnose kopieren
about-collecting = Wird gesammelt…
about-copied = Kopiert

notify-mictest-title = Mikrofontest
notify-mictest-failed-title = Mikrofontest fehlgeschlagen
notify-mictest-recording-body = Aufnahme läuft { $secs } Sekunden – sprechen Sie in Ihr Mikrofon…
notify-mictest-playing-raw-body = Wiedergabe der Rohaufnahme (ohne HushMic)…
notify-mictest-playing-clean-body = Wiedergabe der bereinigten Aufnahme (was Ihre Anwendungen hören)…
notify-mictest-finished-body = Mikrofontest abgeschlossen – die zweite Wiedergabe sollte deutlich weniger Hintergrundgeräusche gehabt haben.
notify-mictest-cancelled-body = Angehalten – die Audioeinstellungen haben sich während des Tests geändert.
notify-mictest-blocked-running = Es läuft bereits ein Mikrofontest.
notify-mictest-blocked-disabled = Schalten Sie zuerst die Rauschunterdrückung ein – der Test vergleicht das rohe Mikrofonsignal mit der bereinigten HushMic-Ausgabe.
notify-mictest-blocked-no-node = Das virtuelle HushMic-Mikrofon ist noch nicht bereit – versuchen Sie es in ein paar Sekunden erneut.
notify-mictest-blocked-no-probe = PipeWire konnte nicht nach Mikrofonen abgefragt werden – läuft PipeWire?
notify-no-feeder-body = Das Mikrofon, das HushMic speist, wurde nicht gefunden.
notify-old-pipewire-body = Die Live-A/B-Ansicht benötigt ein neueres PipeWire auf diesem System.
notify-window-open-body = Das A/B-Testfenster ist bereits geöffnet.
notify-window-fallback-body = Das Testfenster konnte nicht gestartet werden – stattdessen läuft der reine Audio-Mikrofontest.

notify-tray-failed-title = HushMic konnte nicht starten
notify-tray-failed-body = Es konnte kein Symbol im Systembereich registriert werden ({ $error }). Installieren Sie unter GNOME die Erweiterung „AppIndicator and KStatusNotifierItem Support“; KDE und die meisten anderen Desktops bringen das von Haus aus mit.

notify-chain-failed-summary = HushMic konnte das virtuelle Mikrofon nicht starten
notify-chain-stuck-summary = HushMic verliert das virtuelle Mikrofon immer wieder
notify-chain-stuck-body = Das Neuerstellen schlägt wiederholt fehl – läuft PipeWire? Führen Sie `hushmic --tray` in einem Terminal aus, um Details zu sehen.
notify-chain-flapping-summary = HushMic startet das virtuelle Mikrofon immer wieder neu
notify-chain-flapping-body = Es ist in den letzten Minuten mehrfach ausgefallen und neu erstellt worden – die Audiokonfiguration ist möglicherweise instabil. Führen Sie `hushmic --tray` in einem Terminal aus, um Details zu sehen.
notify-running-again-summary = HushMic läuft wieder
notify-running-again-body = Das virtuelle Mikrofon ist wieder verfügbar.

notify-reroute-summary = Eine andere Anwendung leitet HushMics Mikrofon um
notify-reroute-body = Ein laufendes Audio-Werkzeug (EasyEffects?) leitet HushMics Eingang immer wieder auf sich selbst um. Deaktivieren Sie in EasyEffects „Process All Input Streams“ (oder schließen Sie hushmic_input aus) und starten Sie HushMic anschließend neu.

notify-mic-fallback-body = Ihr Mikrofon wurde getrennt – HushMic folgt vorerst dem Systemstandard.
notify-mic-return-body = Ihr Mikrofon ist wieder da – HushMic verwendet es wieder.

notify-shortcuts-title = HushMic-Tastenkürzel
notify-shortcuts-body = Dieser Desktop kann den Tastenkürzel-Editor von hier aus nicht öffnen. Suchen Sie HushMic in den Tastenkürzel-Einstellungen Ihres Systems, um die Tasten zu ändern.

notify-autostart-denied-title = Autostart wurde nicht erlaubt
notify-autostart-denied-body = Der Desktop hat HushMics Autostart-Anfrage abgelehnt. Sie können ihn in den Systemeinstellungen unter Anwendungsberechtigungen / Hintergrund-Apps erlauben.
