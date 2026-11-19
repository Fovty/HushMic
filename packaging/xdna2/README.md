# Optional DPDFNet XDNA2 host package

Build the host service with `cargo build --locked --release -p
dpdfnet-xdna2 --features host`. Build the LADSPA adapter with the
`xdna2` feature. The CPU-only LADSPA package can use its usual build.

Install the service binary as `/usr/bin/dpdfnet-xdna2-service`, its D-Bus
activation file under the session service directory, and its systemd
unit under the user unit directory. The host catalog is
`/usr/share/dpdfnet-xdna2/providers.json`. Use version 1 and bind each
model identity to a digest-verified spectral library and manifest.
Record live-audio and automatic qualification separately. The D-Bus
interface is `io.github.fovty.HushMic.XDNA2.V1`; the service name is
`io.github.fovty.HushMic.XDNA2`.

The session bus starts the service on demand. The service opens one
private inference owner at a time. Client audio uses a bounded Unix
packet socket; only the service's worker loads XRT. An audio deadline
allows CPU fallback while a separate supervision thread waits for the
worker to release the XRT session. The broker refuses another session
until the previous worker exits. Idle LADSPA streams close after two
seconds and open a fresh session when capture resumes. A package upgrade
can remove acceleration without changing the standard LADSPA audio or
control ports.

`HUSHMIC_XDNA2_PROVIDERS` and `HUSHMIC_XDNA2_DEVELOPMENT=1` belong to a
separate test service. The latter admits an unqualified profile only
under explicitly selected Prefer NPU. Production packages rely on the
qualified catalog and normal user permissions.
