tray-title = HushMic
tray-title-bypass = HushMic (Bypass)
tray-title-muted = HushMic (stumm)

tray-mode-suppress = Rauschunterdrückung
tray-mode-bypass = Bypass
tray-mode-mute = Stumm
tray-mode-off = Aus

tray-microphone = Mikrofon
tray-system-default = Systemstandard

tray-strength-medium = Mittel
tray-strength-strong = Stark
tray-strength-maximum = Maximal

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

notify-chain-failed-summary = HushMic konnte das virtuelle Mikrofon nicht starten
notify-chain-stuck-summary = HushMic verliert das virtuelle Mikrofon immer wieder
notify-chain-stuck-body = Das Neuerstellen schlägt wiederholt fehl – läuft PipeWire? Führen Sie `hushmic --doctor` in einem Terminal aus, um Details zu sehen.
notify-chain-flapping-summary = HushMic startet das virtuelle Mikrofon immer wieder neu
notify-chain-flapping-body = Es ist in den letzten Minuten mehrfach ausgefallen und neu erstellt worden – die Audiokonfiguration ist möglicherweise instabil. Führen Sie `hushmic --doctor` in einem Terminal aus, um Details zu sehen.
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
